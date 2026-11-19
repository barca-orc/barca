//! The one Tokio runtime of the process, built when it is first needed.
//!
//! barca-core is async-native and runs on whatever runtime the caller provides. A command
//! needs one event loop; jobs execute in Python processes and blocking I/O uses Tokio's
//! separate blocking pool. `barca serve` also handles concurrent HTTP requests, so it gets
//! worker threads. `version`, `docs` and `tokens` never build one.

use crate::error::{CliError, CliErrorExt, Context};

pub(crate) struct Runtime {
    json: bool,
    serving: bool,
    built: Option<tokio::runtime::Runtime>,
}

impl Runtime {
    /// A runtime for a command whose errors print as `json` says; `serving` is `barca serve`.
    pub(crate) fn lazy(json: bool, serving: bool) -> Self {
        Self {
            json,
            serving,
            built: None,
        }
    }

    /// The runtime, built on the first call. A failure to build it ends the process (infra).
    pub(crate) fn get(&mut self) -> &tokio::runtime::Runtime {
        let (json, serving) = (self.json, self.serving);
        self.built.get_or_insert_with(|| {
            let mut builder = if serving {
                let mut builder = tokio::runtime::Builder::new_multi_thread();
                builder.worker_threads(2);
                builder
            } else {
                tokio::runtime::Builder::new_current_thread()
            };
            builder.enable_all().build().unwrap_or_else(|e| {
                CliError::from_barca(
                    barca_core::BarcaError::Other(format!("failed to create runtime: {e}")),
                    &Context::default(),
                )
                .emit(json)
            })
        })
    }

    /// End it before the process exits. `barca serve` has drained and is about to exit:
    /// tearing its runtime down would only race its background loops (a timer polled during
    /// that panics), so it is left as it is.
    pub(crate) fn finish(self) {
        if self.serving {
            std::mem::forget(self.built);
        }
    }
}
