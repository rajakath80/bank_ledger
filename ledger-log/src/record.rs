//! Log records: the durable, versioned envelope around each command.
//!
//! Every record ever written must remain decodable by every future version
//! of this code -- a stronger constraint than a normal wire format where
//! old messages are eventually allowed to age out. Records are versioned;
//! fields may be added, but never removed or reinterpreted, which is why
//! `RecordV1`'s shape is frozen from the moment the first record is
//! written. A future change gets a `RecordV2` variant, not an edit to this
//! one.

use ledger_core::Command;
use ledger_types::time::LogTime;
use serde::{Deserialize, Serialize};

/// The state machine version records in this log were produced under.
/// Stamped once per log via [`LogHeader`]. If a future bugfix changes what
/// replay produces, either the old behaviour is preserved behind a branch
/// keyed on this version, or there is an explicit snapshot-and-freeze
/// cutover after which history before it is never replayed.
pub const STATE_MACHINE_VERSION: u32 = 1;

/// Written once, at the start of a log.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LogHeader {
    pub state_machine_version: u32,
}

impl Default for LogHeader {
    fn default() -> Self {
        LogHeader { state_machine_version: STATE_MACHINE_VERSION }
    }
}

/// One record in the log. An enum from the start, so a future `V2` variant
/// can be added without touching how `V1` records already on disk decode.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub enum Record {
    V1(RecordV1),
}

/// A command paired with the log time it was committed at. This shape is
/// frozen: see the module docs.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct RecordV1 {
    pub at: LogTime,
    pub command: Command,
}

impl Record {
    pub fn v1(at: LogTime, command: Command) -> Record {
        Record::V1(RecordV1 { at, command })
    }

    pub fn at(&self) -> LogTime {
        match self {
            Record::V1(r) => r.at,
        }
    }

    pub fn command(&self) -> &Command {
        match self {
            Record::V1(r) => &r.command,
        }
    }
}
