//! `ledger-log`: the log's record format, storage boundary, and in-memory
//! replay.
//!
//! The log is the source of truth; `State` (in `ledger-core`) is a
//! projection of it. This crate owns three things: the versioned envelope
//! around each command (`record`, `encoding`), the byte framing and
//! storage trait real durability plugs into (`frame`, `storage`), and
//! folding `apply` over a sequence of records to reconstruct a `State`
//! (`replay`).
//!
//! `ledger-store` (real segment files, fsync, crash recovery) implements
//! [`RecordStorage`] against actual files; [`InMemoryStorage`] here is the
//! same trait with no durability at all, used as the fast default and as
//! the test double for anything that doesn't need to touch a disk.

pub mod encoding;
pub mod frame;
pub mod query;
pub mod record;
pub mod replay;
pub mod storage;

pub use encoding::{
    DecodeError, EncodeError, decode, decode_log, decode_payload, encode, encode_log,
    encode_payload,
};
pub use frame::{decode_frames, encode_frame};
pub use query::{entries_for_account, entry_for_idempotency_key};
pub use record::{LogHeader, Record, RecordV1, STATE_MACHINE_VERSION};
pub use replay::replay;
pub use storage::{InMemoryStorage, RecordStorage, RecoveryOutcome};
