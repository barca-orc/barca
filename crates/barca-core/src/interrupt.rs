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

/// How long a cancelled run may spend pushing its record to the shared history.
///
/// A push of the metadata database takes tens of milliseconds to a directory store and well
/// under two seconds to an object store, so ten seconds covers a slow link and one conflict
/// retry (a pull and a second push). It is also about as long as someone who pressed Ctrl-C
/// will wait before pressing it again, which ends the wait anyway.
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
}
