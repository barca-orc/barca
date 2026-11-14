//! Facts about processes and this machine that every layer shares: whether a pid is alive,
//! this host's name, and [`Until`], when a wait for a helper or a lock gives up.

use crate::BarcaError;
use tokio_util::sync::CancellationToken;

/// This machine's host name ("" when it cannot be read). Stored on a run row next to the pid:
/// a database restored from another machine's backup holds that machine's runs, whose pids
/// mean nothing here.
pub fn local_host() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `buf.len()` bytes into `buf`.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

/// True unless the process is known to be gone. (A pid reused by an unrelated process reads as
/// alive: the run then stays `running`, which is what it was before pids were recorded.)
pub fn pid_alive(pid: i64) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return true;
    };
    if pid <= 0 {
        return true;
    }
    // SAFETY: signal 0 sends nothing; it only checks that the pid exists.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

// ─── Deadlines and cancellation ──────────────────────────────────────────────

/// When a helper or a lock wait gives up: `cancel` fires, or `deadline` passes. Either way the
/// helper is stopped and the result is [`BarcaError::Cancelled`].
#[derive(Clone, Copy, Default)]
pub struct Until<'a> {
    pub cancel: Option<&'a CancellationToken>,
    pub deadline: Option<std::time::Instant>,
}

impl<'a> Until<'a> {
    pub fn check(self) -> Result<(), BarcaError> {
        if self.cancel.is_some_and(CancellationToken::is_cancelled)
            || self.remaining().is_some_and(|left| left.is_zero())
        {
            Err(BarcaError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Only wrap cancellable admission, never an admitted database mutation.
    pub async fn admit<T>(
        self,
        work: impl std::future::Future<Output = T>,
    ) -> Result<T, BarcaError> {
        self.check()?;
        let cancelled = async {
            match self.cancel {
                Some(cancel) => cancel.cancelled().await,
                None => std::future::pending().await,
            }
        };
        let expired = async {
            match self.remaining() {
                Some(left) => tokio::time::sleep(left).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            biased;
            _ = cancelled => Err(BarcaError::Cancelled),
            _ = expired => Err(BarcaError::Cancelled),
            result = work => Ok(result),
        }
    }

    /// Never: the transfer runs to its end.
    pub fn done() -> Self {
        Self::default()
    }

    pub fn cancelled(cancel: &'a CancellationToken) -> Self {
        Self {
            cancel: Some(cancel),
            deadline: None,
        }
    }

    /// The time left, when there is a deadline (zero once it has passed).
    pub fn remaining(&self) -> Option<std::time::Duration> {
        self.deadline
            .map(|d| d.saturating_duration_since(std::time::Instant::now()))
    }
}
