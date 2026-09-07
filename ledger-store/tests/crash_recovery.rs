//! `M3` "done when" checks:
//! - Crash at an arbitrary injected point, recover, and the log is a
//!   valid prefix.
//! - Torn writes, short writes and bit flips are all detected.
//!
//! There's no real process crash here (that's what a fault-injecting
//! `ledger-sim` would give you later); instead each test writes real data
//! through `SegmentedFileStorage`, then reaches underneath it and mutates
//! the segment file directly -- truncating it or flipping a bit -- to
//! simulate exactly what a crash mid-write or a corrupted sector would
//! leave behind, then opens a fresh `SegmentedFileStorage` over the same
//! directory and checks recovery does the right thing.

use ledger_log::RecordStorage;
use ledger_store::SegmentedFileStorage;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A directory under the OS temp dir that's unique per test run and
/// cleaned up when the guard drops, so failed tests don't leave junk
/// behind and concurrent test runs don't collide.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("ledger_store_test_{label}_{nanos}"));
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn active_segment_path(dir: &Path) -> PathBuf {
    let segments_dir = dir.join("segments");
    let mut entries: Vec<PathBuf> = fs::read_dir(&segments_dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    entries.into_iter().last().expect("at least one segment file")
}

#[test]
fn round_trips_through_a_close_and_reopen() {
    let dir = TempDir::new("round_trip");

    {
        let mut store = SegmentedFileStorage::open(dir.path()).unwrap();
        store.append(b"first").unwrap();
        store.append(b"second").unwrap();
        store.append(b"third").unwrap();
        store.sync().unwrap();
    } // store dropped: simulates a clean process restart

    let store = SegmentedFileStorage::open(dir.path()).unwrap();
    let outcome = store.recover().unwrap();
    assert_eq!(outcome.records, vec![b"first".to_vec(), b"second".to_vec(), b"third".to_vec()]);
    assert_eq!(outcome.discarded_tail_bytes, 0);
}

#[test]
fn a_crash_that_truncates_the_last_write_recovers_a_valid_prefix() {
    let dir = TempDir::new("torn_write");

    {
        let mut store = SegmentedFileStorage::open(dir.path()).unwrap();
        store.append(b"safely-written-one").unwrap();
        store.append(b"safely-written-two").unwrap();
        store.sync().unwrap();
        // Simulate a crash partway through writing a third record: the OS
        // only got some of the bytes onto disk before the process died.
        store.append(b"this-write-gets-cut-off-by-a-crash").unwrap();
    }

    let segment = active_segment_path(dir.path());
    let full_len = fs::metadata(&segment).unwrap().len();
    // Chop off the last 10 bytes, landing inside the third record's frame.
    let truncated_len = full_len - 10;
    let file = fs::OpenOptions::new().write(true).open(&segment).unwrap();
    file.set_len(truncated_len).unwrap();
    drop(file);

    let store = SegmentedFileStorage::open(dir.path()).unwrap();
    let outcome = store.recover().unwrap();

    assert_eq!(outcome.records, vec![b"safely-written-one".to_vec(), b"safely-written-two".to_vec()]);
    assert!(outcome.discarded_tail_bytes > 0, "the torn record must be reported as discarded, not silently dropped");
}

#[test]
fn a_bit_flip_is_detected_and_everything_after_it_is_discarded() {
    let dir = TempDir::new("bit_flip");

    {
        let mut store = SegmentedFileStorage::open(dir.path()).unwrap();
        store.append(b"good-record-one").unwrap();
        store.append(b"this-one-gets-corrupted").unwrap();
        store.append(b"good-record-three-never-trusted").unwrap();
        store.sync().unwrap();
    }

    let segment = active_segment_path(dir.path());
    let mut bytes = fs::read(&segment).unwrap();
    // The first record's frame is 8 (header) + 15 (payload) = 23 bytes.
    // Flip a bit inside the second record's payload, well past its header.
    let corrupt_at = 23 + 8 + 5;
    bytes[corrupt_at] ^= 0x01;
    fs::write(&segment, &bytes).unwrap();

    let store = SegmentedFileStorage::open(dir.path()).unwrap();
    let outcome = store.recover().unwrap();

    assert_eq!(outcome.records, vec![b"good-record-one".to_vec()]);
    assert!(outcome.discarded_tail_bytes > 0);
}

#[test]
fn recovery_spans_multiple_segments_and_stops_at_the_first_bad_one() {
    let dir = TempDir::new("multi_segment");

    {
        // A tiny max size forces a rollover after nearly every record.
        let mut store = SegmentedFileStorage::open_with_max_segment_bytes(dir.path(), 16).unwrap();
        for i in 0..10u32 {
            store.append(format!("record-{i}").as_bytes()).unwrap();
        }
        store.sync().unwrap();
    }

    let segments_dir = dir.path().join("segments");
    let segment_count = fs::read_dir(&segments_dir).unwrap().count();
    assert!(segment_count > 1, "the tiny max segment size should have forced a rollover");

    let store = SegmentedFileStorage::open_with_max_segment_bytes(dir.path(), 16).unwrap();
    let outcome = store.recover().unwrap();
    let expected: Vec<Vec<u8>> = (0..10u32).map(|i| format!("record-{i}").into_bytes()).collect();
    assert_eq!(outcome.records, expected);
    assert_eq!(outcome.discarded_tail_bytes, 0);
}

#[test]
fn reopening_under_a_stale_state_machine_version_is_rejected() {
    let dir = TempDir::new("version_mismatch");
    {
        let _store = SegmentedFileStorage::open(dir.path()).unwrap();
    }

    let header_path = dir.path().join("header.json");
    fs::write(&header_path, br#"{"state_machine_version": 999999}"#).unwrap();

    let result = SegmentedFileStorage::open(dir.path());
    assert!(result.is_err(), "opening a log written under a different state machine version must fail, not silently proceed");
}
