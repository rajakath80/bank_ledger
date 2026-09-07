use ledger_types::money::{Amount, Eur, Usd};

fn main() {
    let usd = Amount::<Usd>::from_minor_units(100);
    let eur = Amount::<Eur>::from_minor_units(100);
    let _ = usd + eur;
}
