//! The balance data model: addressable, phase-aware balances held gross
//! (credit and debit separately), keyed by (account, address, denom,
//! phase). An account does not have a single balance; it has a map of
//! these.

use crate::ids::{AccountId, AddressId, DenomId};
use serde::{Deserialize, Serialize};

/// Where a balance sits in a two-phase transfer.
///
/// Declaration order is significant: it defines the derived `Ord`
/// (`Committed < PendingIn < PendingOut`), which in turn determines the
/// lower bound used by [`BalanceKey::account_lower_bound`]. `Committed`
/// must stay the first variant.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum Phase {
    /// Settled, available (subject to product-defined availability rules).
    Committed,
    /// Authorised inbound, not yet settled.
    PendingIn,
    /// Authorised outbound (e.g. a hold), not yet settled.
    PendingOut,
}

/// The key of one addressable balance.
///
/// Field order matters: it defines the derived `Ord`, and deliberately
/// puts `account` first so that every balance belonging to one account is
/// contiguous in an ordered map. `State::account_is_zero` (in
/// `ledger-core`) relies on this to range-scan a single account without
/// touching the rest of the ledger -- validation stays account-local even
/// for that check.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct BalanceKey {
    pub account: AccountId,
    pub address: AddressId,
    pub denom: DenomId,
    pub phase: Phase,
}

impl BalanceKey {
    /// The smallest `BalanceKey` for `account` under `Ord` -- the start of
    /// the contiguous range of every balance that account holds.
    pub fn account_lower_bound(account: AccountId) -> BalanceKey {
        BalanceKey {
            account,
            address: AddressId(0),
            denom: DenomId(0),
            phase: Phase::Committed,
        }
    }

    /// One past the largest possible `BalanceKey` for `account`: the
    /// exclusive upper bound of that same range. Wraps at `u64::MAX`
    /// rather than panicking; a wrapped bound is still a correct (if
    /// degenerate) empty-range marker for that edge case.
    pub fn account_upper_bound(account: AccountId) -> BalanceKey {
        BalanceKey {
            account: AccountId(account.0.wrapping_add(1)),
            address: AddressId(0),
            denom: DenomId(0),
            phase: Phase::Committed,
        }
    }
}

/// A gross balance. Credit and debit are held separately rather than
/// netted, because gross turnover is required for reporting and
/// reconciliation and cannot be recovered from a net figure.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Balance {
    pub credit: i128,
    pub debit: i128,
}

impl Balance {
    pub const ZERO: Balance = Balance {
        credit: 0,
        debit: 0,
    };

    /// Net value under the credit-normal convention (credit increases,
    /// debit decreases). What "increases availability" means for a given
    /// address is a product decision; the ledger only offers this
    /// arithmetic net.
    pub fn net(self) -> i128 {
        self.credit - self.debit
    }

    /// Whether this balance is net zero -- the sense relevant to account
    /// closure. An account that has been credited 500 and debited 500 has
    /// non-zero gross turnover but zero net value, and must still be
    /// closeable; closure cares about outstanding value, not history.
    pub fn is_zero(self) -> bool {
        self.net() == 0
    }

    pub fn apply_credit(self, amount: u128) -> Balance {
        Balance {
            credit: self.credit + amount as i128,
            debit: self.debit,
        }
    }

    pub fn apply_debit(self, amount: u128) -> Balance {
        Balance {
            credit: self.credit,
            debit: self.debit + amount as i128,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balance_key_ordering_groups_by_account_first() {
        let a1 = BalanceKey {
            account: AccountId(1),
            address: AddressId(99),
            denom: DenomId(99),
            phase: Phase::PendingOut,
        };
        let a2 = BalanceKey {
            account: AccountId(2),
            address: AddressId(0),
            denom: DenomId(0),
            phase: Phase::Committed,
        };
        assert!(
            a1 < a2,
            "every key for account 1 must sort before any key for account 2"
        );
    }

    #[test]
    fn account_bounds_capture_exactly_that_accounts_keys() {
        let lower = BalanceKey::account_lower_bound(AccountId(5));
        let upper = BalanceKey::account_upper_bound(AccountId(5));
        let inside = BalanceKey {
            account: AccountId(5),
            address: AddressId(u32::MAX),
            denom: DenomId(u16::MAX),
            phase: Phase::PendingOut,
        };
        let outside = BalanceKey {
            account: AccountId(6),
            address: AddressId(0),
            denom: DenomId(0),
            phase: Phase::Committed,
        };

        assert!(lower <= inside && inside < upper);
        assert!(outside >= upper);
    }

    #[test]
    fn credit_and_debit_are_tracked_separately() {
        let b = Balance::ZERO.apply_credit(100).apply_debit(40);
        assert_eq!(
            b,
            Balance {
                credit: 100,
                debit: 40
            }
        );
        assert_eq!(b.net(), 60);
        assert!(!b.is_zero());
    }
}
