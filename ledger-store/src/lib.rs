//! `ledger-store`: real durable storage.
//!
//! Segment files under a directory, each record framed with a length and
//! CRC32 (see `ledger_log::frame`), explicit `fsync` on every `sync()`
//! call, and torn-tail truncation on recovery. Implements
//! [`ledger_log::RecordStorage`], so anything written against the trait --
//! including a real ledger's write and recovery path -- works unchanged
//! whether it's pointed at [`ledger_log::InMemoryStorage`] in a test or
//! [`SegmentedFileStorage`] here.
//!
//! # Layout
//!
//! ```text
//! <dir>/
//!   header.json       -- LogHeader, written once, version-checked on open
//!   segments/
//!     00000000.seg
//!     00000001.seg
//!     ...              -- sequential, sorted lexically by zero-padded index
//! ```
//!
//! Segments are named by a monotonically increasing sequence number, not
//! by log position: this crate works in raw framed bytes and has no idea
//! what a `LogTime` is -- that's `ledger-log`/`ledger-core`'s concern,
//! deliberately kept out of the storage boundary. A segment rolls over
//! once it exceeds `max_segment_bytes`.
//!
//! # Recovery
//!
//! Segments are read in order. Within each segment, [`ledger_log::frame`]
//! decodes frames until it hits a torn write (not enough bytes left for
//! the header or the payload it claims) or a checksum mismatch (a bit
//! flip). The moment that happens, in any segment, recovery stops -- a
//! bad frame invalidates everything written after it in the *whole* log,
//! not just the rest of that one file, since segments were written in
//! order and a later segment can't be trusted if an earlier one is torn.

use ledger_log::{LogHeader, RecordStorage, RecoveryOutcome, decode_frames, encode_frame};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const DEFAULT_MAX_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;

pub struct SegmentedFileStorage {
    dir: PathBuf,
    max_segment_bytes: u64,
    active_index: u32,
    active_file: File,
    active_len: u64,
}

impl SegmentedFileStorage {
    /// Open (creating if necessary) a segmented store rooted at `dir`,
    /// with the default segment size.
    pub fn open(dir: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_with_max_segment_bytes(dir, DEFAULT_MAX_SEGMENT_BYTES)
    }

    /// Open (creating if necessary) a segmented store rooted at `dir`,
    /// rolling to a new segment once the active one exceeds
    /// `max_segment_bytes`. A small value is useful in tests, to exercise
    /// multi-segment recovery without writing tens of megabytes.
    pub fn open_with_max_segment_bytes(
        dir: impl AsRef<Path>,
        max_segment_bytes: u64,
    ) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let segments_dir = dir.join("segments");
        fs::create_dir_all(&segments_dir)?;

        write_or_check_header(&dir)?;

        let active_index = latest_segment_index(&segments_dir)?.unwrap_or(0);
        let active_path = segment_path(&segments_dir, active_index);
        let active_file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(&active_path)?;
        let active_len = active_file.metadata()?.len();

        Ok(SegmentedFileStorage {
            dir,
            max_segment_bytes,
            active_index,
            active_file,
            active_len,
        })
    }

    fn segments_dir(&self) -> PathBuf {
        self.dir.join("segments")
    }

    fn roll_over_if_needed(&mut self) -> io::Result<()> {
        if self.active_len < self.max_segment_bytes {
            return Ok(());
        }
        self.active_file.sync_all()?;
        self.active_index += 1;
        let path = segment_path(&self.segments_dir(), self.active_index);
        self.active_file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)?;
        self.active_len = 0;
        Ok(())
    }
}

impl RecordStorage for SegmentedFileStorage {
    #[tracing::instrument(skip(self, payload), fields(payload_bytes = payload.len(), segment = self.active_index))]
    fn append(&mut self, payload: &[u8]) -> io::Result<()> {
        self.roll_over_if_needed()?;
        let framed = encode_frame(payload);
        self.active_file.write_all(&framed)?;
        self.active_len += framed.len() as u64;
        Ok(())
    }

    #[tracing::instrument(skip(self), fields(segment = self.active_index))]
    fn sync(&mut self) -> io::Result<()> {
        // `sync_all` flushes both data and metadata -- deliberately not
        // `sync_data`: a crash that loses the metadata update (e.g. the
        // file's recorded length) can itself masquerade as a torn write on
        // recovery even if every data byte made it to disk.
        self.active_file.sync_all()
    }

    #[tracing::instrument(skip(self))]
    fn recover(&self) -> io::Result<RecoveryOutcome> {
        // Delegates to the free function below rather than duplicating
        // the logic: recovery never touches `self.active_file` at all,
        // so there's no reason for it to require a write-capable handle
        // to already be open.
        recover_read_only(&self.dir)
    }
}

/// Read every segment file under `dir` and decode it, exactly like
/// [`RecordStorage::recover`] on a [`SegmentedFileStorage`] -- but without
/// ever opening anything for writing, or requiring the directory to be
/// writable at all. This is what a genuinely read-only consumer (a
/// forensics tool, `ledger-guardian`, anything mounting the ledger
/// directory `:ro`) should call instead of `SegmentedFileStorage::open`,
/// which unconditionally opens the active segment in append+write mode
/// even if the caller never intends to write -- exactly the mismatch that
/// makes `open` fail against a read-only mount for a purely-reading
/// caller.
///
/// Unlike `open`'s header handling, a missing `header.json` is an error
/// here, not something to create: a read-only consumer has no business
/// inventing a fresh ledger where none exists.
pub fn recover_read_only(dir: impl AsRef<Path>) -> io::Result<RecoveryOutcome> {
    let dir = dir.as_ref();
    check_header_read_only(dir)?;

    let segments_dir = dir.join("segments");
    let mut indices = list_segment_indices(&segments_dir)?;
    indices.sort_unstable();

    let mut records = Vec::new();
    let mut discarded_tail_bytes = 0usize;

    for index in indices {
        let path = segment_path(&segments_dir, index);
        let mut file = File::open(&path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;

        let (segment_records, consumed) = decode_frames(&bytes);
        records.extend(segment_records);
        let tail = bytes.len() - consumed;

        if tail > 0 {
            discarded_tail_bytes = tail;
            tracing::warn!(
                segment = index,
                discarded_tail_bytes = tail,
                "torn or corrupt frame found; stopping recovery here"
            );
            break;
        }
    }

    tracing::debug!(
        record_count = records.len(),
        discarded_tail_bytes,
        "read-only recovery completed"
    );
    Ok(RecoveryOutcome {
        records,
        discarded_tail_bytes,
    })
}

fn check_header_read_only(dir: &Path) -> io::Result<()> {
    let header_path = dir.join("header.json");
    let bytes = fs::read(&header_path)?; // missing file is correctly an error here, not something to create
    let on_disk: LogHeader = serde_json::from_slice(&bytes)?;
    let expected = LogHeader::default();
    if on_disk.state_machine_version != expected.state_machine_version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "state machine version mismatch: this log was written under version {}, the running code is version {}",
                on_disk.state_machine_version, expected.state_machine_version,
            ),
        ));
    }
    Ok(())
}

fn write_or_check_header(dir: &Path) -> io::Result<()> {
    let header_path = dir.join("header.json");
    let expected = LogHeader::default();

    if !header_path.exists() {
        let bytes = serde_json::to_vec_pretty(&expected)?;
        fs::write(&header_path, bytes)?;
        return Ok(());
    }

    let bytes = fs::read(&header_path)?;
    let on_disk: LogHeader = serde_json::from_slice(&bytes)?;
    if on_disk.state_machine_version != expected.state_machine_version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "state machine version mismatch: this log was written under version {}, the running code is version {}",
                on_disk.state_machine_version, expected.state_machine_version,
            ),
        ));
    }
    Ok(())
}

fn segment_path(segments_dir: &Path, index: u32) -> PathBuf {
    segments_dir.join(format!("{index:08}.seg"))
}

fn list_segment_indices(segments_dir: &Path) -> io::Result<Vec<u32>> {
    let mut indices = Vec::new();
    for entry in fs::read_dir(segments_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(index) = name
            .strip_suffix(".seg")
            .and_then(|stem| stem.parse::<u32>().ok())
        {
            indices.push(index);
        }
    }
    Ok(indices)
}

fn latest_segment_index(segments_dir: &Path) -> io::Result<Option<u32>> {
    let mut indices = list_segment_indices(segments_dir)?;
    indices.sort_unstable();
    Ok(indices.last().copied())
}
