//! `ledger-tags`: product and category tagging, kept deliberately outside
//! `ledger-core`.
//!
//! The ledger itself "assigns no meaning" to an `AddressId` or which
//! product an `AccountId` belongs to (design section 2) -- that decision
//! stands, and this crate is where the meaning actually gets assigned,
//! for the one consumer (FinOps) that needs it. `ledger-core` never
//! depends on this crate; the boundary is one-directional.
//!
//! # Two independent dimensions, not one
//!
//! It's tempting to tag *addresses* as belonging to a product (e.g.
//! "address 0 is the savings product's principal"), but that's wrong:
//! `AddressId`s like `DEFAULT`/`PRINCIPAL` are a shared, per-institution
//! enumeration reused across every product's accounts -- a checking
//! account and a savings account both post to address 0 for their main
//! balance. Tagging the address itself would incorrectly claim every
//! account's default balance belongs to whichever product tagged it
//! first.
//!
//! The correct split:
//! - **Account -> product** (`AccountTag`): which product a given account
//!   belongs to. This is the classification FinOps actually needs for
//!   "what's the volume for the savings product" -- filter postings by
//!   the account they touched, not the address.
//! - **Address -> label** (`AddressLabel`): a human label for what an
//!   address represents (e.g. "principal", "interest_accrued", "fees"),
//!   shared meaning across every product, used only for finer breakdowns
//!   within a product's volume, never for classifying which product a
//!   posting belongs to.
//!
//! # Why JSON, not TOML
//!
//! Recent versions of the `toml` crate pull in dependencies (`indexmap`
//! via `toml_edit`) that require Cargo's `edition2024` feature, unstable
//! before Cargo 1.85 -- the same class of problem `ledger-log::encoding`
//! hit and avoided by choosing JSON over a binary format. `serde_json` is
//! already a dependency everywhere else in this workspace and has no such
//! issue, so the tag file here is JSON too, editable by hand without
//! recompiling anything.

use ledger_types::ids::{AccountId, AddressId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

/// One account's product assignment, as it appears in the tag file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountTag {
    pub account: u64,
    pub product: String,
}

/// One address's category label, as it appears in the tag file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressLabel {
    pub address: u32,
    pub label: String,
}

/// The on-disk shape of a tag file: two independent lists, either of
/// which may be omitted.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct TagFile {
    #[serde(default)]
    accounts: Vec<AccountTag>,
    #[serde(default)]
    addresses: Vec<AddressLabel>,
}

/// A loaded set of tags, indexed for fast lookup in both directions.
#[derive(Clone, Debug, Default)]
pub struct ProductTags {
    account_product: HashMap<u64, String>,
    address_label: HashMap<u32, String>,
}

impl ProductTags {
    /// Load tags from a JSON file shaped like:
    /// ```json
    /// {
    ///   "accounts": [{ "account": 1, "product": "savings" }],
    ///   "addresses": [{ "address": 0, "label": "principal" }]
    /// }
    /// ```
    /// Either list may be empty or omitted entirely.
    pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        let file: TagFile = serde_json::from_slice(&bytes)?;
        Ok(Self::from_file(file))
    }

    fn from_file(file: TagFile) -> Self {
        ProductTags {
            account_product: file
                .accounts
                .into_iter()
                .map(|t| (t.account, t.product))
                .collect(),
            address_label: file
                .addresses
                .into_iter()
                .map(|t| (t.address, t.label))
                .collect(),
        }
    }

    /// The product an account belongs to, if tagged. `None` means "this
    /// account exists on the ledger but nobody has assigned it to a
    /// product" -- this type has no way to know whether the account
    /// exists at all (that's `ledger-core::State`'s job, not this one's).
    pub fn product_for_account(&self, account: AccountId) -> Option<&str> {
        self.account_product.get(&account.0).map(String::as_str)
    }

    /// The category label for an address, if one has been assigned.
    /// Shared across every product -- see the module docs for why this is
    /// a separate map from `product_for_account`, not the same one.
    pub fn label_for_address(&self, address: AddressId) -> Option<&str> {
        self.address_label.get(&address.0).map(String::as_str)
    }

    /// Every account tagged as belonging to `product`.
    pub fn accounts_for_product<'a>(
        &'a self,
        product: &'a str,
    ) -> impl Iterator<Item = AccountId> + 'a {
        self.account_product
            .iter()
            .filter(move |(_, p)| p.as_str() == product)
            .map(|(&id, _)| AccountId(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_tag_file(contents: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        // `std::process::id()` alone is NOT unique here: Rust runs tests
        // in parallel by default, all within the same process, so every
        // test sharing just the PID would race to read/write/delete the
        // same file. Nanosecond timestamp plus a monotonic counter makes
        // each call's path unique even if two tests happen to call this
        // in the same nanosecond.
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "ledger_tags_test_{}_{nanos}_{n}.json",
            std::process::id()
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn loads_and_looks_up_both_maps() {
        let path = write_temp_tag_file(
            r#"{
                "accounts": [
                    { "account": 1, "product": "savings" },
                    { "account": 2, "product": "savings" },
                    { "account": 10, "product": "checking" }
                ],
                "addresses": [
                    { "address": 0, "label": "principal" },
                    { "address": 2, "label": "interest_accrued" }
                ]
            }"#,
        );

        let tags = ProductTags::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(tags.product_for_account(AccountId(1)), Some("savings"));
        assert_eq!(tags.product_for_account(AccountId(10)), Some("checking"));
        assert_eq!(tags.product_for_account(AccountId(999)), None);

        assert_eq!(tags.label_for_address(AddressId(0)), Some("principal"));
        assert_eq!(
            tags.label_for_address(AddressId(2)),
            Some("interest_accrued")
        );
        assert_eq!(tags.label_for_address(AddressId(99)), None);

        let mut savings: Vec<u64> = tags.accounts_for_product("savings").map(|a| a.0).collect();
        savings.sort_unstable();
        assert_eq!(savings, vec![1, 2]);
    }

    #[test]
    fn missing_lists_default_to_empty() {
        let path =
            write_temp_tag_file(r#"{ "accounts": [{ "account": 1, "product": "savings" }] }"#);
        let tags = ProductTags::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        assert_eq!(tags.product_for_account(AccountId(1)), Some("savings"));
        assert_eq!(tags.label_for_address(AddressId(0)), None);
    }

    #[test]
    fn an_address_tagged_for_one_product_does_not_leak_into_another() {
        // The regression this crate's whole split exists to prevent:
        // address 0 is used by both a savings and a checking account, and
        // tagging it once must not make every account "the same product".
        let path = write_temp_tag_file(
            r#"{
                "accounts": [
                    { "account": 1, "product": "savings" },
                    { "account": 2, "product": "checking" }
                ],
                "addresses": [
                    { "address": 0, "label": "principal" }
                ]
            }"#,
        );
        let tags = ProductTags::load(&path).unwrap();
        std::fs::remove_file(&path).ok();

        // Address 0 has exactly one label, shared, regardless of which
        // account posted to it -- it never implies a product on its own.
        assert_eq!(tags.label_for_address(AddressId(0)), Some("principal"));
        assert_eq!(tags.product_for_account(AccountId(1)), Some("savings"));
        assert_eq!(tags.product_for_account(AccountId(2)), Some("checking"));
    }
}
