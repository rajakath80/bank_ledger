//! The storage trait: the one place IO is allowed to cross into the log.
//!
//! Nothing in `ledger-core` touches a file descriptor at all, and nothing
//! in `ledger-log` outside this trait does either. `ledger-store` (real
//! segment files) implements it for production use; [`InMemoryStorage`]
//! below implements it for tests. A future fault-injecting fake
//! (`ledger-sim`) substitutes for either without any other code changing --
//! that substitutability is the entire point of putting IO behind a trait
//! "from the first commit" (design section 1).

use crate::frame::{decode_frames, encode_frame};

/// A durable (or durable-simulating) store of framed records.
pub trait RecordStorage {
    /// Append one record's payload bytes. The implementation frames it
    /// (length + checksum) -- callers never construct a frame themselves.
    fn append(&mut self, payload: &[u8]) -> std::io::Result<()>;

    /// Force everything appended so far to durable storage. Must not
    /// return before the data is actually durable; an implementation that
    /// lies about this is exactly the fault a fault-injecting test double
    /// exists to model later.
    fn sync(&mut self) -> std::io::Result<()>;

    /// Read back every valid record, in order, stopping at the first torn
    /// or corrupt frame.
    fn recover(&self) -> std::io::Result<RecoveryOutcome>;
}

/// The result of [`RecordStorage::recover`].
#[derive(Debug, PartialEq, Eq)]
pub struct RecoveryOutcome {
    /// Payload bytes of every good record, in order.
    pub records: Vec<Vec<u8>>,
    /// How many trailing bytes were discarded as torn or corrupt. Zero
    /// means the store ended cleanly on a frame boundary.
    pub discarded_tail_bytes: usize,
}

/// A durability-free in-memory [`RecordStorage`]. This is not a mock: it
/// is the exact test implementation the build plan calls for -- "the
/// in-memory log from M2 becomes the test implementation for free" -- now
/// expressed against the trait instead of a bespoke `Vec<Record>`.
#[derive(Default)]
pub struct InMemoryStorage {
    bytes: Vec<u8>,
}

impl InMemoryStorage {
    pub fn new() -> Self {
        InMemoryStorage::default()
    }
}

impl RecordStorage for InMemoryStorage {
    fn append(&mut self, payload: &[u8]) -> std::io::Result<()> {
        self.bytes.extend(encode_frame(payload));
        Ok(())
    }

    fn sync(&mut self) -> std::io::Result<()> {
        // Nothing to do: it's already "durable" in the sense that matters
        // for an in-process test -- it never disappears mid-process.
        Ok(())
    }

    fn recover(&self) -> std::io::Result<RecoveryOutcome> {
        let (records, consumed) = decode_frames(&self.bytes);
        Ok(RecoveryOutcome { records, discarded_tail_bytes: self.bytes.len() - consumed })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_everything_that_was_appended_and_synced() {
        let mut storage = InMemoryStorage::new();
        storage.append(b"a").unwrap();
        storage.append(b"b").unwrap();
        storage.sync().unwrap();

        let outcome = storage.recover().unwrap();
        assert_eq!(outcome.records, vec![b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(outcome.discarded_tail_bytes, 0);
    }
}
