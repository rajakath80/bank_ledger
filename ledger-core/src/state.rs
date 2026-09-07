//! The ledger state: a pure, persistent snapshot.
//!
//! `State` never mutates in place -- every command produces a *new* `State`
//! that shares structure with the old one via `im::OrdMap`'s structural
//! sharing. That is deliberate from the first commit: it is what makes
//! copy-on-write snapshots and MVCC reads possible later at no additional
//! cost, and starting with a plain `HashMap` and converting afterwards
//! would be a rewrite of everything that touches state. `OrdMap` (rather
//! than `im::HashMap`) is used specifically so iteration order is
//! deterministic, per the workspace-wide "no HashMap iteration in anything
//! affecting state or output ordering" rule.

use std::collections::BTreeMap;

use im::OrdMap;
use ledger_types::balance::{Balance, BalanceKey};
use ledger_types::ids::{AccountId, ClientKey, DenomId};
use ledger_types::posting::Batch;
use ledger_types::time::LogTime;
use serde::{Deserialize, Serialize};

use crate::effect::Effect;
use crate::rejection::Rejection;

/// Whether an account is open. Closure requires every balance address to
/// be zero; the account row is then kept (marked closed) rather than
/// deleted -- a deleted key could later be silently reopened, which would
/// look indistinguishable from the same account continuing.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct AccountMeta {
    pub open: bool,
}

/// The outcome of a previously-applied batch, cached so a retry under the
/// same idempotency key can return it verbatim instead of being
/// revalidated.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum BatchOutcome {
    Applied(Vec<Effect>),
    Rejected(Rejection),
}

/// A cached idempotency-key entry. `recorded_at` is a `LogTime`, never a
/// wall-clock timestamp, so eviction (see `State::evict_dedup_before`)
/// can be driven deterministically by log position during replay.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DedupEntry {
    pub recorded_at: LogTime,
    pub batch: Batch,
    pub outcome: BatchOutcome,
}

/// The full ledger state. `State::new()` (equivalently `State::default())
/// is the genesis state: no accounts, no balances, no dedup history.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub accounts: OrdMap<AccountId, AccountMeta>,
    pub balances: OrdMap<BalanceKey, Balance>,
    pub dedup: OrdMap<ClientKey, DedupEntry>,
}

impl State {
    pub fn new() -> State {
        State::default()
    }

    /// True if `account` has been opened and has not since been closed.
    pub fn is_open(&self, account: AccountId) -> bool {
        matches!(self.accounts.get(&account), Some(meta) if meta.open)
    }

    /// True if `account` has ever been opened, whether or not it is
    /// currently closed.
    pub fn exists(&self, account: AccountId) -> bool {
        self.accounts.contains_key(&account)
    }

    /// True if every balance address held by `account` -- across every
    /// denomination and phase -- nets to zero credit and debit.
    /// This is a single contiguous range scan of the ordered balance map
    /// (see `BalanceKey`'s field order), never a full-table scan: it
    /// staysaccount-local even though it is, in effect, a check over
    /// all of this account's balances".
    pub fn account_is_zero(&self, account: AccountId) -> bool {
        let lower = BalanceKey::account_lower_bound(account);
        let upper = BalanceKey::account_upper_bound(account);
        self.balances
            .range(lower..upper)
            .all(|(_, balance)| balance.is_zero())
    }

    /// Evict dedup entries recorded at or before `cutoff`. `cutoff` must
    /// be a `LogTime` derived from log position -- never a wall-clock
    /// value -- so that eviction is itself replay-deterministic. Not
    /// called automatically by `apply`; a caller (e.g. a periodic
    /// maintenance command) drives it explicitly.
    pub fn evict_dedup_before(&self, cutoff: LogTime) -> State {
        let mut next = self.clone();
        let to_evict: Vec<ClientKey> = next
            .dedup
            .iter()
            .filter(|(_, entry)| entry.recorded_at <= cutoff)
            .map(|(key, _)| *key)
            .collect();
        for key in to_evict {
            next.dedup.remove(&key);
        }
        next
    }

    /// A canonical byte encoding of this state, suitable for a
    /// byte-for-byte equality check (see `ledger-log`'s
    /// replay-determinism test, and the snapshot-checksum requirement in
    /// a later milestone). `serde_json` cannot serialise a JSON *object*
    /// keyed by a non-string type, and `BalanceKey`/`AccountId`
    /// `ClientKey` are structs, not strings -- so this does not derive
    /// `Serialize` on `State` directly and serialise that. Instead each
    /// `OrdMap` is flattened to a `Vec` of `(key, value)` pairs first
    /// That flattening is exactly as deterministic as the map itself:
    /// `OrdMap::iter` already yields entries in `Ord` order, so two
    /// states with the same content always flatten to the same sequence,
    /// which is the property this function exists to make checkable.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        #[derive(Serialize)]
        struct Canonical<'a> {
            accounts: Vec<(&'a AccountId, &'a AccountMeta)>,
            balances: Vec<(&'a BalanceKey, &'a Balance)>,
            dedup: Vec<(&'a ClientKey, &'a DedupEntry)>,
        }

        let canonical = Canonical {
            accounts: self.accounts.iter().collect(),
            balances: self.balances.iter().collect(),
            dedup: self.dedup.iter().collect(),
        };

        serde_json::to_vec(&canonical)
            .expect("canonical state encoding is infallible: all fields are plain data")
    }

    /// Every balance address held by `account`, across every denomination
    /// and phase, as `(key, balance)` pairs. Built for read-side tools
    /// (e.g. an account-balance lookup) that want the whole picture for
    /// one account rather than a single `(address, denom, phase)` lookup.
    ///
    /// Reuses the same contiguous range scan as `account_is_zero` -- an
    /// account-local read, not a full-ledger scan.
    pub fn balances_for_account(&self, account: AccountId) -> Vec<(BalanceKey, Balance)> {
        let lower = BalanceKey::account_lower_bound(account);
        let upper = BalanceKey::account_upper_bound(account);
        self.balances
            .range(lower..upper)
            .map(|(k, v)| (*k, *v))
            .collect()
    }

    /// The net value of every open balance, summed per denomination,
    /// across the whole ledger. Zero for every denomination is the
    /// invariant `apply` maintains on every write (see
    /// `ledger-core/tests/property_zero_sum.rs`) -- this is a read-side
    /// way to assert that invariant on demand, e.g. for a reconciliation
    /// tool, rather than only checking it in tests.
    pub fn total_net_per_denom(&self) -> BTreeMap<DenomId, i128> {
        let mut totals: BTreeMap<DenomId, i128> = BTreeMap::new();
        for (key, balance) in self.balances.iter() {
            *totals.entry(key.denom).or_insert(0) += balance.net();
        }
        totals
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use ledger_types::balance::Phase;
    use ledger_types::ids::{AddressId, DenomId};

    #[test]
    fn genesis_state_has_nothing_open() {
        let state = State::new();
        assert!(!state.exists(AccountId(1)));
        assert!(!state.is_open(AccountId(1)));
    }

    #[test]
    fn account_is_zero_ignores_other_accounts() {
        let mut state = State::new();
        let key_other = BalanceKey {
            account: AccountId(2),
            address: AddressId::DEFAULT,
            denom: DenomId(1),
            phase: Phase::Committed,
        };
        state.balances.insert(
            key_other,
            Balance {
                credit: 500,
                debit: 0,
            },
        );
        assert!(state.account_is_zero(AccountId(1)));
    }
}
