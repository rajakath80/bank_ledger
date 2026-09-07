//! Identifiers used throughout the ledger. All are small, `Copy`, totally
//! ordered newtypes, so they can be used directly as map keys with
//! deterministic iteration order -- ordering matters here, not just
//! equality, because `BalanceKey`'s derived `Ord` (see `balance.rs`) is
//! load-bearing for account-local range scans.

use serde::{Deserialize, Serialize};

/// Identifies an account. Opaque to the ledger beyond equality and
/// ordering -- the ledger assigns no meaning to the value itself.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct AccountId(pub u64);

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct AddressId(pub u32);

impl AddressId {
    /// The address every account has by default, used by tests and by any
    /// caller that has not yet declared a richer address manifest.
    pub const DEFAULT: AddressId = AddressId(0);
}

/// Identifies a currency / denomination at the runtime level -- the id
/// carried inside `BalanceKey`. Client-facing code should prefer the
/// type-checked [`crate::money::Amount<C>`], which converts to this id at
/// the boundary where money enters the ledger core.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct DenomId(pub u16);

/// A client-supplied idempotency key attached to a [`crate::posting::Batch`].
/// Two batches submitted under the same key are the same logical request:
/// a retry with an identical body is a no-op that returns the original
/// outcome; a retry with a different body is a hard rejection, never a
/// silent replay of the new body.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct ClientKey(pub u128);
