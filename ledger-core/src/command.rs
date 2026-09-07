//! Commands accepted by `apply`.

use ledger_types::{ids::AccountId, posting::Batch};
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Command {
    OpenAccount {
        account: AccountId,
    },
    CloseAccount {
        account: AccountId,
    },
    PostBatch(Batch),
    /// A no-op that advances the log's notion of time. Schedule firings
    /// and other time-driven effects enter the log as commands like this
    /// one -- never as a direct call from a wall-clock timer into the
    /// state machine, which would destroy replay determinism.
    Tick,
}
