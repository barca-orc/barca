//! Refusals a route makes before it does anything: on a `--read-only` server, and while the
//! server drains.

use crate::config::ServeConfig;
use crate::error::ApiError;
use crate::state::Lifecycle;

/// Refuse a request while the server drains before it stops: `503` with `Retry-After`
/// (`POST /state/reset`). Runs are not refused: they join the queue, which the next server
/// resumes.
pub(crate) fn refuse_if_draining(
    lifecycle: &Lifecycle,
    config: &ServeConfig,
) -> Result<(), ApiError> {
    if lifecycle.is_draining() {
        return Err(ApiError::Unavailable {
            message: "this server is shutting down: it finishes the runs in flight and \
                      accepts no new ones; retry against the server that replaces it"
                .to_string(),
            retry_after: retry_after_secs(config),
        });
    }
    Ok(())
}

/// Refuse a request that would run or cancel work on a `--read-only` server.
pub(crate) fn refuse_if_read_only(config: &ServeConfig) -> Result<(), ApiError> {
    if config.read_only {
        return Err(ApiError::Forbidden(
            "this server is read-only (`barca serve --read-only`): it does not run or cancel work"
                .to_string(),
        ));
    }
    Ok(())
}

/// The seconds a client refused during a drain is told to wait (`Retry-After`): the drain
/// timeout, by when this server is gone and its replacement is likely up. At least 1.
pub(crate) fn retry_after_secs(config: &ServeConfig) -> u64 {
    config.resolved.drain_timeout_secs.max(1)
}
