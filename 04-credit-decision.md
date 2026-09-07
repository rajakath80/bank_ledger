# Credit Decisioning: Design

Companion to `02-internal-design.md` / `03-build-plan.md`. Describes a
credit decisioning engine built on top of the existing ledger, and the
loan product it eventually disburses into. Two separate concerns are
addressed here and kept separate throughout: **the decision** (one-time,
advisory, produces reasons) and **the product** (ongoing, produces
postings). Conflating them is the most common design mistake in systems
like this, and section 6 explains why.

---

## 1. Scope

In scope:
- A transparent, rule-based credit score combining real internal signals
  (from the ledger) with synthetic external signals (a mock bureau file).
- Adverse-action-style reason codes for every decision, not just a
  pass/fail.
- An audit log of every decision made, independent of the ledger.
- The origination posting that turns an approved decision into money
  moving.

Out of scope, deliberately:
- Machine-learned scoring. A formula you can read beats a model you
  can't, and real lending decisions carry explainability obligations
  (Reg B) that a black box can't satisfy even in a toy system.
- Enforcing the decision. Nothing here stops an account from moving money
  regardless of what was decided -- see section 6.
- A WASM contract host for the loan product. See section 7 for why this
  is deferred, not rejected.
- Real external data of any kind. There is no credit bureau connection.
  Every synthetic value is labelled as such at the point it's returned,
  the same convention FinOps's infrastructure-cost numbers already use.

## 2. What lenders actually evaluate

Underwriting conventionally organizes around five factors (the "5 Cs").
Mapping each to what this system can honestly compute:

| Factor | Question | Source here |
|---|---|---|
| Capacity | Can they repay? | **Real**: detected income (recurring same-size credits at regular intervals -- the same cash-flow-underwriting technique real fintech lenders built on top of Plaid-style bank data actually use). **Synthetic**: debts held elsewhere, needed for a true DTI ratio, which this system cannot see. |
| Capital | Do they have reserves? | **Real**: current balance, trailing average balance, balance volatility over a lookback window. |
| Character | Do they pay obligations reliably? | **Real**: overdraft frequency / total time spent negative -- meaningful specifically because `apply()` never blocks a negative balance (design section 5), so this is genuine, not fabricated, risk signal. **Synthetic**: payment history on obligations *elsewhere*, which by definition cannot come from one bank's own ledger. |
| Collateral | What secures the loan? | Not applicable to an unsecured personal loan -- omitted rather than faked. |
| Conditions | Amount, purpose, term | Application input, not computed. |

Every "Real" row above is computed by replaying this ledger's own
history; every "Synthetic" row is a value from a mock external-data file,
returned with `"synthetic": true` attached, mirroring FinOps's
`get_infrastructure_cost` convention exactly.

One further real-world detail worth reproducing: U.S. lenders are legally
required to give specific reasons for a decline (Equal Credit Opportunity
Act, implemented via Regulation B). Building the score as a small set of
named, ranked factors -- rather than a single opaque number -- produces
those reason codes as a natural byproduct of the scoring method, not as
an afterthought bolted on for compliance flavor.

## 3. Data model

### 3.1 Internal signals (real)

Computed by replaying the ledger (`recover_read_only` + `replay`,
exactly as Guardian/FinOps already do) and scanning the account's own
postings -- never another account's, preserving the same account-local
validation discipline `ledger-core::apply` itself follows:

```
InternalSignals {
    account_id: u64,
    current_balance_net: i128,
    average_balance_90d: f64,          // requires real ValueTime -- see 3.3
    days_overdrawn_90d: u32,
    overdraft_count_90d: u32,
    detected_recurring_credit: Option<{ amount: u128, interval_days: u32 }>,
}
```

### 3.2 External signals (synthetic, explicit)

A flat JSON file, same pattern and same reasoning as `ledger-tags`'s
`products.json` (JSON over TOML: no `edition2024` dependency risk; a
file, not a service, because there is no real service to call):

```json
{
  "accounts": [
    { "account_id": 1, "mock_bureau_score": 580, "mock_external_dti": 0.55 },
    { "account_id": 2, "mock_bureau_score": 720, "mock_external_dti": 0.15 },
    { "account_id": 3, "mock_bureau_score": 640, "mock_external_dti": 0.38 },
    { "account_id": 4, "mock_bureau_score": 690, "mock_external_dti": 0.25 },
    { "account_id": 5, "mock_bureau_score": 510, "mock_external_dti": 0.62 }
  ]
}
```

Every value from this file is wrapped before it ever reaches a decision:

```
ExternalSignals { synthetic: true, mock_bureau_score: u32, mock_external_dti: f64 }
```

### 3.3 The `ValueTime(0)` problem

Every example built so far (`seed_ledger`, `durable_ledger`,
`simulate_incidents`) posts with `ValueTime(0)` as a placeholder. Account
tenure and any lookback-window signal (90-day average balance, 90-day
overdraft count) are meaningless until real dates are threaded through
`ValueTime`. This is called out explicitly rather than silently producing
a plausible-looking but fake "90 days" number: **step 1's seed data must
use real, spread-out `ValueTime` values**, or the lookback-window signals
report on step accordingly (either compute over "all available history"
and label the window as such, or refuse to compute and say why).

### 3.4 Decision record (the audit trail)

Appended to `ledger-credit`'s own append-only log via the same
`RecordStorage` trait TechOps's incident log already uses -- a separate
store, never touching the ledger's own segments, for the same reason
TechOps's incidents don't either:

```
Decision {
    decision_id: u128,
    account_id: u64,
    requested_amount: u128,
    decided_at_unix_ms: u128,
    outcome: Approved { terms: LoanTerms } | Declined,
    score: u32,
    reason_codes: Vec<String>,        // top factors, ranked
    internal_signals: InternalSignals,
    external_signals: ExternalSignals, // synthetic: true always present
}
```

Storing the full signal snapshot alongside the outcome, not just the
outcome, is what makes `explain_decision` possible later without
re-deriving history that may have since changed.

## 4. Scoring: transparent and rule-based

A weighted point system, not a model. Each factor contributes a bounded
number of points; the total maps to approve/decline and a suggested rate
tier; the factors are then ranked by contribution and the lowest-scoring
ones become the reason codes on a decline. Exact weights are a starting
point to be tuned once step 1 exists to tune against, not a claim of
calibrated real-world accuracy -- nothing about this system should imply
otherwise.

## 5. Architecture: plain endpoints, not MCP

Established directly against this project's own precedent before writing
any code. `get_product_volume` and friends (FinOps) are MCP because the
dashboard's chat lets a model choose to call them from an open-ended
question -- but `overview_handler`'s scheduled refresh calls the same
tools directly, with no model involved, proving MCP was never required
for the computation itself.

`evaluate_credit_application(account_id, amount)` has exactly this
shape: a known operation with known parameters, invoked by a button, not
composed on the fly from natural language. It is built as a plain Axum
endpoint, the same way `/api/accounts` and `/api/transfers` already are,
for the same reason. If a conversational "why was this declined" /
"what if the amount were smaller" capability is wanted later,
`explain_decision` is the one piece of this system that's a genuine MCP
fit -- exploratory, benefits from a model chaining calls -- and can be
added as a *second* exposure of the same underlying Rust functions
without duplicating them, exactly as FinOps's tool functions could
equally be called directly today.

## 6. Decision vs. enforcement -- a boundary, stated once, held everywhere

Approving a loan produces exactly one artifact right now: a `Decision`
record and, if approved, a single `PostBatch` (see section 7.1). Nothing
in `ledger-core` knows a credit limit exists, checks one, or blocks a
transfer because of one. This mirrors the ledger's own founding
principle -- "the ledger does not know what a fee, a mortgage, or
interest is" -- applied to credit decisions too: the decision *engine*
knows about creditworthiness; the *ledger* only ever knows about
postings. If real-time enforcement (block a withdrawal that would exceed
an approved limit) is ever wanted, that is new scope, not an extension of
what's described here, and should be its own design pass.

## 7. The loan product, once approved

### 7.1 Origination

One ordinary `PostBatch`, no different in kind from any transfer already
built: debit an internal `loans_issued` account, credit the borrower's
`PRINCIPAL` address. Zero-sum holds exactly as it does for any other
batch.

### 7.2 Interest accrual

Computed **outside** the ledger, in plain, readable, testable Rust --
not inside a contract, not inside the ledger. On whatever schedule is
chosen, the accrual amount is calculated externally and posted as a
normal `PostBatch` crediting `INTEREST_ACCRUED`. The ledger records the
result of a calculation; it does not perform the calculation. This is
exactly the division of responsibility `ledger-tags` already established
for product labels: the ledger assigns no meaning, an external, legible
layer does.

### 7.3 Repayment

Borrower's account debited; amount allocated across `PRINCIPAL` /
`INTEREST_ACCRUED` / `FEES` by ordinary postings, following whatever
allocation order the product defines (interest-first is conventional and
a reasonable default).

### 7.4 Why not WASM (M7) for any of this

M7 exists for the case of *many* products with *dynamically definable*
accrual logic that must be tamper-proof enforced by the ledger itself
without trusting external orchestration -- real weight, bought for a
real problem. Learning one loan product end to end has neither multiple
products nor an adversarial-orchestrator threat model. Building the WASM
host now would be exactly the premature complexity the ledger's own
design doc warns against in its first principle ("anything that is a
response to a measured problem waits until the problem is measured").
Revisit if a second, meaningfully different product ever needs logic
the first one's code can't already express by parameterizing it.

## 8. Build order

1. `ledger-credit`: internal signals + external mock file + scorecard +
   reason codes + audit log, as plain Axum endpoints. **(this doc's
   companion build starts here)**
2. Loan mechanics: origination, an amortization schedule calculator, a
   manual "post next installment" action -- plain Rust, posting through
   existing `apply`/`RecordStorage`, no ledger changes.
3. Wire an approved decision to the origination posting.