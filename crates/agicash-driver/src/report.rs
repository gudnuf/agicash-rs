//! [`SweepReport`] + per-row outcomes returned by [`run_sweep`].
//!
//! The report is the only output of a sweep. Callers (and Lane B's
//! trigger task) read it to know:
//! - how many rows were seen,
//! - how many advanced (the React `useMutation onSuccess` analog),
//! - how many were benign no-ops (already resolved),
//! - how many failed (transient retries exhausted, or fatal domain
//!   error).
//!
//! [`run_sweep`]: crate::run_sweep

use uuid::Uuid;

use crate::error::ClassifiedError;

/// Which kind of row a per-row outcome refers to. Lets a caller assert
/// "the `send_swap` DRAFT row I seeded advanced" without re-fetching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    SendSwapDraft,
    SendSwapPending,
    ReceiveSwapPending,
    MintQuoteUnpaid,
    MintQuotePaid,
    MintQuoteExpire,
    MeltQuotePending,
    MeltQuoteUnpaidExpire,
}

/// Per-row outcome — one entry per row the sweep tried to advance.
#[derive(Debug)]
pub struct RowOutcome {
    pub row_id: Uuid,
    pub kind: RowKind,
    pub status: RowStatus,
}

/// What happened to the row this sweep.
#[derive(Debug)]
pub enum RowStatus {
    /// The service method ran and advanced (or confirmed terminal —
    /// some service methods short-circuit on terminal as `Ok`, which is
    /// indistinguishable from "advanced this attempt" without diffing
    /// state; for driver purposes that's a successful sweep step
    /// either way).
    Advanced,
    /// The service method recognised the row as already-resolved /
    /// wrong-state and returned a classified benign error. No change,
    /// no concern — the row is no longer stuck.
    NoOp { reason: String },
    /// The row failed after exhausting retries (Transient) or on the
    /// first attempt (Fatal). Left for the next sweep.
    Failed {
        error: ClassifiedError,
        attempts: u8,
    },
}

/// Summary of one sweep over the user's pending state.
///
/// `rows_seen == advanced + no_op + failed`. The counts let a test
/// assert "I seeded N stuck rows; sweep advanced K of them" without
/// walking the per-row vec.
#[derive(Debug, Default)]
pub struct SweepReport {
    pub rows_seen: usize,
    pub advanced: usize,
    pub no_op: usize,
    pub failed: usize,
    /// Per-row outcomes, in the order rows were processed. Useful for
    /// targeted asserts ("the DRAFT row I seeded advanced") and for
    /// surfacing the *which* of a failure.
    pub outcomes: Vec<RowOutcome>,
}

impl SweepReport {
    /// `true` iff every row reached a terminal status this sweep —
    /// either advanced or a benign no-op. Failures (transient or
    /// fatal) make this `false`; the next trigger will re-sweep.
    #[must_use]
    pub fn all_clean(&self) -> bool {
        self.failed == 0
    }

    pub(crate) fn record(&mut self, outcome: RowOutcome) {
        self.rows_seen += 1;
        match &outcome.status {
            RowStatus::Advanced => self.advanced += 1,
            RowStatus::NoOp { .. } => self.no_op += 1,
            RowStatus::Failed { .. } => self.failed += 1,
        }
        self.outcomes.push(outcome);
    }
}
