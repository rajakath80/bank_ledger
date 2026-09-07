//! The state machine's single entry point.

use std::collections::BTreeMap;

use ledger_types::ids::DenomId;
use ledger_types::posting::{Batch, Side};
use ledger_types::time::LogTime;

use crate::command::Command;
use crate::effect::Effect;
use crate::rejection::Rejection;
use crate::state::{AccountMeta, BatchOutcome, DedupEntry, State};

/// A short, non-sensitive descriptor of a command's shape, safe to record
/// on a tracing span. Deliberately does not include account ids, amounts,
/// or idempotency keys -- those are financial detail that a general
/// latency/tracing consumer (e.g. a future TechOps tool) has no business
/// seeing by default; `ledger-log`'s query functions are the sanctioned
/// path for anyone who actually needs "what did this posting contain."
fn command_kind(cmd: &Command) -> &'static str {
    match cmd {
        Command::OpenAccount { .. } => "OpenAccount",
        Command::CloseAccount { .. } => "CloseAccount",
        Command::PostBatch(_) => "PostBatch",
        Command::Tick => "Tick",
    }
}

/// Apply one command to `state` at log time `at`, producing either a new
/// state and the effects it produced, or a rejection. On rejection the
/// caller simply keeps the `state` it already had -- nothing is mutated in
/// place, so there is nothing to roll back.
///
/// Pure: no IO, no clock, no randomness. Given the same `(state, cmd, at)`
/// this always returns the same result -- the property the whole
/// replay/simulation story in `ledger-log` and `ledger-sim` depends on.
///
/// Instrumented with a `tracing` span (`kind`, `log_time`, and on
/// completion `outcome`/`rejection`). `tracing` is a pure facade with no
/// IO of its own -- with no subscriber registered, these calls compile
/// down to a cheap disabled-check and do nothing, so this does not violate
/// "no IO in ledger-core". A caller (e.g. `ledger-guardian`, or a future
/// write-path service) opts into actually recording anything by
/// installing a `tracing-subscriber` -- this crate never does that itself.
#[tracing::instrument(skip(state, cmd), fields(kind = command_kind(cmd), log_time = at.0))]
pub fn apply(state: &State, cmd: &Command, at: LogTime) -> Result<(State, Vec<Effect>), Rejection> {
    let result = match cmd {
        Command::OpenAccount { account } => {
            if state.exists(*account) {
                Err(Rejection::AccountAlreadyExists)
            } else {
                let mut next = state.clone();
                next.accounts.insert(*account, AccountMeta { open: true });
                Ok((next, vec![Effect::AccountOpened(*account)]))
            }
        }

        Command::CloseAccount { account } => {
            if !state.exists(*account) {
                Err(Rejection::CloseUnknownAccount)
            } else if !state.is_open(*account) {
                // Already closed: closing again is a no-op success rather
                // than an error, so replaying a duplicate close command is
                // stable instead of flip-flopping on rejection.
                Ok((state.clone(), vec![]))
            } else if !state.account_is_zero(*account) {
                Err(Rejection::AccountNotEmpty)
            } else {
                let mut next = state.clone();
                next.accounts.insert(*account, AccountMeta { open: false });
                Ok((next, vec![Effect::AccountClosed(*account)]))
            }
        }

        Command::PostBatch(batch) => apply_batch(state, batch, at),

        Command::Tick => Ok((state.clone(), vec![Effect::Ticked(at)])),
    };

    match &result {
        Ok((_, effects)) => tracing::debug!(
            outcome = "applied",
            effect_count = effects.len(),
            "apply completed"
        ),
        Err(rejection) => tracing::warn!(outcome = "rejected", ?rejection, "apply completed"),
    }

    result
}

fn apply_batch(
    state: &State,
    batch: &Batch,
    at: LogTime,
) -> Result<(State, Vec<Effect>), Rejection> {
    if let Some(existing) = state.dedup.get(&batch.idempotency_key) {
        return if &existing.batch == batch {
            // Same key, same body: this is a retry. Return the original
            // outcome verbatim without touching the state again.
            match &existing.outcome {
                BatchOutcome::Applied(effects) => Ok((state.clone(), effects.clone())),
                BatchOutcome::Rejected(rejection) => Err(*rejection),
            }
        } else {
            // Same key, different body: not a retry, a conflicting
            // request. Hard rejection regardless of whether the original
            // attempt succeeded.
            Err(Rejection::DuplicateIdempotencyKeyConflict)
        };
    }

    let outcome = validate_and_build_effects(state, batch);

    let mut next = state.clone();
    let dedup_outcome = match &outcome {
        Ok(effects) => BatchOutcome::Applied(effects.clone()),
        Err(rejection) => BatchOutcome::Rejected(*rejection),
    };
    next.dedup.insert(
        batch.idempotency_key,
        DedupEntry {
            recorded_at: at,
            batch: batch.clone(),
            outcome: dedup_outcome,
        },
    );

    match outcome {
        Ok(effects) => {
            for posting in &batch.postings {
                let current = next.balances.get(&posting.key).copied().unwrap_or_default();
                let updated = match posting.side {
                    Side::Credit => current.apply_credit(posting.amount),
                    Side::Debit => current.apply_debit(posting.amount),
                };
                next.balances.insert(posting.key, updated);
            }
            Ok((next, effects))
        }
        Err(rejection) => {
            // The dedup entry still records the rejection -- a retry with
            // the same body must see the same rejection rather than being
            // revalidated -- but no balances move, and `next` (containing
            // only the dedup insert) is what the caller receives so the
            // rejection is itself replay-idempotent.
            Err(rejection)
        }
    }
}

/// Account-local validation: every check here reads only the batch itself
/// and the *existence/openness* of the accounts it references -- never
/// another account's balances. This constraint is what lets validation
/// shard by account later; it must not erode as more checks are added.
fn validate_and_build_effects(state: &State, batch: &Batch) -> Result<Vec<Effect>, Rejection> {
    if batch.postings.is_empty() {
        return Err(Rejection::EmptyBatch);
    }

    for posting in &batch.postings {
        if !state.is_open(posting.key.account) {
            return Err(Rejection::AccountNotFound);
        }
    }

    let mut net_by_denom: BTreeMap<DenomId, i128> = BTreeMap::new();
    for posting in &batch.postings {
        let signed = match posting.side {
            Side::Credit => posting.amount as i128,
            Side::Debit => -(posting.amount as i128),
        };
        *net_by_denom.entry(posting.key.denom).or_insert(0) += signed;
    }
    if net_by_denom.values().any(|&net| net != 0) {
        return Err(Rejection::UnbalancedBatch);
    }

    Ok(vec![Effect::BatchApplied {
        idempotency_key: batch.idempotency_key,
        postings_applied: batch.postings.len(),
    }])
}
