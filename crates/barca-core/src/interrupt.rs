//! How a run is told to stop.
//!
//! Ctrl-C has two stages, and the coordinator alone acts on them (its helpers are deaf to the
//! signal, see [`crate::helper_proc`]):
//!
//! 1. The first cancels the run ([`Interrupt::cancel`]). Workers and transfers are stopped,
//!    what finished is recorded, and the run wraps up: it pushes its record to the shared
//!    history, for at most [`WRAP_UP_LIMIT`].
//! 2. A second one abandons the wrap-up ([`Interrupt::abandon`]): the push is stopped at once.
//!    Nothing is lost by that. The record is in the local history, and a later run on this
//!    machine uploads it.
//!
//! Either way the command ends as cancelled (exit 130).

use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// How long a cancelled run may spend backing its history up to the store (RFC-0009).
///
/// A backup of the metadata database takes tens of milliseconds to a directory store and well
/// under two seconds to an object store, so ten seconds covers a slow link. It is also about
/// as long as someone who pressed Ctrl-C will wait before pressing it again, which ends the
/// wait anyway.
pub const WRAP_UP_LIMIT: Duration = Duration::from_secs(10);

/// The two stop signals of a run. A plain [`CancellationToken`] converts into one whose
/// wrap-up is never abandoned, only bounded by [`WRAP_UP_LIMIT`].
#[derive(Clone, Debug, Default)]
pub struct Interrupt {
    /// Cancelled by the first Ctrl-C (or by whoever started the run): stop the run.
    pub cancel: CancellationToken,
    /// Cancelled by the second Ctrl-C: stop wrapping up as well.
    pub abandon: CancellationToken,
}

impl Interrupt {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one more interrupt: the first cancels, any later one abandons the wrap-up.
    pub fn interrupt(&self) {
        if self.cancel.is_cancelled() {
            self.abandon.cancel();
        } else {
            self.cancel.cancel();
        }
    }
}

impl From<CancellationToken> for Interrupt {
    fn from(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            abandon: CancellationToken::new(),
        }
    }
}

/// Why a run was cancelled, when whoever cancels it knows: `barca serve` sets
/// `server shutdown` before it cancels the runs a drain did not finish (#190).
///
/// The run reads it when it records itself as cancelled: the reason goes into the run's logs
/// (`[barca] run cancelled: <reason>`) and onto the `barca.run` telemetry span as
/// `barca.reason`. It travels as a task-local value around the run's future
/// ([`CancelReason::scope`]), so the cancellation token stays a plain token. The first reason
/// set is kept.
#[derive(Clone, Debug, Default)]
pub struct CancelReason(std::sync::Arc<std::sync::OnceLock<String>>);

tokio::task_local! {
    static CANCEL_REASON: CancelReason;
}

impl CancelReason {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the reason. Set it before cancelling the run's token. A later call is ignored.
    pub fn set(&self, reason: impl Into<String>) {
        let _ = self.0.set(reason.into());
    }

    pub fn get(&self) -> Option<String> {
        self.0.get().cloned()
    }

    /// Run `fut` (a run) with this reason visible to it through [`cancel_reason`].
    pub async fn scope<F: std::future::Future>(self, fut: F) -> F::Output {
        CANCEL_REASON.scope(self, fut).await
    }
}

/// The reason set for the run executing on this task, if it was started inside
/// [`CancelReason::scope`] and a reason has been set.
pub fn cancel_reason() -> Option<String> {
    CANCEL_REASON.try_with(|r| r.get()).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_interrupt_cancels_and_the_second_abandons() {
        let i = Interrupt::new();
        assert!(!i.cancel.is_cancelled() && !i.abandon.is_cancelled());
        i.interrupt();
        assert!(i.cancel.is_cancelled() && !i.abandon.is_cancelled());
        i.interrupt();
        assert!(i.cancel.is_cancelled() && i.abandon.is_cancelled());
        i.interrupt(); // a third changes nothing
        assert!(i.cancel.is_cancelled() && i.abandon.is_cancelled());
    }

    #[test]
    fn a_run_cancelled_by_a_plain_token_has_a_wrap_up_nobody_abandons() {
        let token = CancellationToken::new();
        let i = Interrupt::from(token.clone());
        token.cancel();
        assert!(i.cancel.is_cancelled() && !i.abandon.is_cancelled());
    }

    #[tokio::test]
    async fn a_cancel_reason_is_seen_inside_its_scope_only_once_set() {
        assert_eq!(cancel_reason(), None, "outside any scope");
        let reason = CancelReason::new();
        let seen = reason.clone().scope(async {
            let before = cancel_reason();
            tokio::task::yield_now().await;
            (before, cancel_reason())
        });
        reason.set("server shutdown");
        reason.set("ignored");
        assert_eq!(
            seen.await,
            (
                Some("server shutdown".into()),
                Some("server shutdown".into())
            )
        );
    }
}
