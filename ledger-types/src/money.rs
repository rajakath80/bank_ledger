//! Money, with the currency encoded in the type so that mixing currencies
//! is a compile error rather than a runtime check.
//!
//! `Amount<C>` is the client-facing, type-checked value. Once an amount
//! crosses into a [`crate::posting::Posting`], the currency degrades to the
//! runtime [`crate::ids::DenomId`] carried in `BalanceKey` -- the ledger
//! core is generic over denomination at runtime and cannot be generic over
//! a distinct Rust type per currency, so the type-level check is a
//! *boundary* guarantee (nothing constructs a mismatched `Amount` sum),
//! not a property the whole system enforces end to end.
//!
//! No floating point appears anywhere in this module, or anywhere in this
//! crate: all amounts are integer minor units (`i128`).

use crate::ids::DenomId;
use std::marker::PhantomData;
use std::ops::{Add, Sub};

/// A currency, implemented by a zero-sized marker type (see [`Usd`],
/// [`Eur`]). `DENOM_ID` is the runtime id an `Amount<Self>` carries once it
/// crosses into the untyped ledger core.
pub trait Currency: Copy + Eq + std::fmt::Debug {
    const DENOM_ID: DenomId;
    const CODE: &'static str;
    /// Minor units per major unit (e.g. 100 for USD/EUR cents).
    const MINOR_UNIT_SCALE: i128;
}

/// An amount of a specific currency, held in integer minor units.
///
/// `Amount<Usd> + Amount<Eur>` does not compile: see
/// `tests/compile_fail/currency_mismatch.rs`, exercised by
/// `tests/currency_mismatch.rs` via `trybuild`.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Amount<C: Currency> {
    minor_units: i128,
    _currency: PhantomData<C>,
}

impl<C: Currency> Amount<C> {
    pub const ZERO: Amount<C> = Amount {
        minor_units: 0,
        _currency: PhantomData,
    };

    pub fn from_minor_units(minor_units: i128) -> Self {
        Amount {
            minor_units,
            _currency: PhantomData,
        }
    }

    pub fn minor_units(self) -> i128 {
        self.minor_units
    }

    pub fn denom_id(self) -> DenomId {
        C::DENOM_ID
    }

    pub fn is_negative(self) -> bool {
        self.minor_units < 0
    }
}

impl<C: Currency> Add for Amount<C> {
    type Output = Amount<C>;
    fn add(self, rhs: Self) -> Self::Output {
        Amount::from_minor_units(self.minor_units + rhs.minor_units)
    }
}

impl<C: Currency> Sub for Amount<C> {
    type Output = Amount<C>;
    fn sub(self, rhs: Self) -> Self::Output {
        Amount::from_minor_units(self.minor_units - rhs.minor_units)
    }
}

/// Rounding policy for an operation that produces a fractional minor unit.
/// Every rounding operation must name one of these explicitly -- there is
/// deliberately no default, and no operation in this crate rounds without
/// one.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RoundingPolicy {
    /// Round towards negative infinity.
    Floor,
    /// Round towards positive infinity.
    Ceil,
    /// Round half to even (banker's rounding); avoids the statistical bias
    /// of always rounding halves the same direction.
    HalfEven,
}

/// Divide `numerator` minor units by `denominator`, applying `policy`, and
/// return `(quotient, residual)` such that
/// `quotient * denominator + residual == numerator`.
///
/// The residual is never dropped by this function -- callers are expected
/// to post it to a real account (a rounding/suspense account) so the trial
/// balance still nets to zero. Returning it explicitly, rather than
/// discarding it, is what makes that possible.
pub fn round_division(numerator: i128, denominator: i128, policy: RoundingPolicy) -> (i128, i128) {
    assert!(
        denominator != 0,
        "round_division: denomitor must not be zero"
    );

    let raw_quotient = numerator / denominator;
    let raw_remainder = numerator % denominator;

    if raw_remainder == 0 {
        return (raw_quotient, 0);
    }

    let same_sign = (raw_remainder < 0) == (denominator < 0);

    let quotient = match policy {
        RoundingPolicy::Floor => {
            if same_sign {
                raw_quotient
            } else {
                raw_quotient - 1
            }
        }
        RoundingPolicy::Ceil => {
            if same_sign {
                raw_quotient + 1
            } else {
                raw_quotient
            }
        }
        RoundingPolicy::HalfEven => {
            let twice_remainder = raw_remainder.unsigned_abs() * 2;
            let denom_abs = denominator.unsigned_abs();
            let round_away_from_zero = match twice_remainder.cmp(&denom_abs) {
                std::cmp::Ordering::Greater => true,
                std::cmp::Ordering::Less => false,
                std::cmp::Ordering::Equal => raw_quotient % 2 != 0,
            };
            if round_away_from_zero {
                if same_sign {
                    raw_quotient + 1
                } else {
                    raw_quotient - 1
                }
            } else {
                raw_quotient
            }
        }
    };

    let residual = numerator - quotient * denominator;
    (quotient, residual)
}

/// USD. Minor unit is the cent.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Usd;
impl Currency for Usd {
    const DENOM_ID: DenomId = DenomId(1);
    const CODE: &'static str = "USD";
    const MINOR_UNIT_SCALE: i128 = 100;
}

/// EUR. Minor unit is the cent.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Eur;
impl Currency for Eur {
    const DENOM_ID: DenomId = DenomId(2);
    const CODE: &'static str = "EUR";
    const MINOR_UNIT_SCALE: i128 = 100;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_currency_amounts_add() {
        let a = Amount::<Usd>::from_minor_units(150);
        let b = Amount::<Usd>::from_minor_units(250);
        assert_eq!((a + b).minor_units(), 400);
    }

    #[test]
    fn round_division_exact_has_zero_residual() {
        assert_eq!(round_division(100, 4, RoundingPolicy::Floor), (25, 0));
        assert_eq!(round_division(100, 4, RoundingPolicy::Ceil), (25, 0));
        assert_eq!(round_division(100, 4, RoundingPolicy::HalfEven), (25, 0));
    }

    #[test]
    fn round_division_floor_rounds_toward_negative_infinity() {
        assert_eq!(round_division(7, 2, RoundingPolicy::Floor), (3, 1));
        assert_eq!(round_division(-7, 2, RoundingPolicy::Floor), (-4, 1));
    }

    #[test]
    fn round_division_ceil_rounds_toward_positive_infinity() {
        assert_eq!(round_division(7, 2, RoundingPolicy::Ceil), (4, -1));
        assert_eq!(round_division(-7, 2, RoundingPolicy::Ceil), (-3, -1));
    }

    #[test]
    fn round_division_half_even_breaks_ties_to_even_quotient() {
        // 5 / 2 = 2.5 -> nearest even quotient is 2.
        assert_eq!(round_division(5, 2, RoundingPolicy::HalfEven), (2, 1));
        // 7 / 2 = 3.5 -> nearest even quotient is 4.
        assert_eq!(round_division(7, 2, RoundingPolicy::HalfEven), (4, -1));
    }

    #[test]
    fn round_division_residual_always_reconstructs_numerator() {
        for numerator in -50i128..50 {
            for denominator in [2i128, 3, 7, -4] {
                for policy in [
                    RoundingPolicy::Floor,
                    RoundingPolicy::Ceil,
                    RoundingPolicy::HalfEven,
                ] {
                    let (quotient, residual) = round_division(numerator, denominator, policy);
                    assert_eq!(quotient * denominator + residual, numerator);
                }
            }
        }
    }
}
