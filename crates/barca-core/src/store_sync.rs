//! A run's connection to a remote store: queue each finished artifact for upload, fetch remote
//! inputs before a phase reads them, and wait for the transfers at the end of the run.

use crate::commands::{BLOCKED_ARTIFACT_PATH, fmt_bytes};
use crate::dispatch;
use crate::recover;
use crate::transfer::{ArtifactLayout, TransferClient};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// This run's link to a separate artifact store: the transfer helper plus
/// the cache hits whose artifacts still live only in the store.
pub(crate) struct StoreSync {
    pub(crate) client: TransferClient,
    pub(crate) layout: ArtifactLayout,
    /// Local mirror path → (node id, store location, recorded SHA-256), for
    /// store-backed cache hits. Fetched on first use, so fully-cached
    /// intermediates a run never reads are never downloaded. With a recorded
    /// hash, a copy already on disk is checked against it on first use too.
    pub(crate) fetchable: HashMap<String, (String, String, Option<String>)>,
    /// Base step id -> what was found, for each store copy fetched in this run that does not
    /// have its recorded hash. Put on the step reports when the run ends ([`crate::mismatch`]).
    pub(crate) mismatched: HashMap<String, String>,
    /// Local mirror paths of fetches the store answered with "no such object",
    /// until [`Self::take_missing`] collects them.
    missing: Vec<String>,
    /// Whether the store itself is there, once it has been asked (see
    /// [`Self::confirm_present`]).
    present: Option<Result<(), String>>,
    /// The run's cancellation: a wait on the store ends when it fires.
    cancel: CancellationToken,
}

/// What a wait on the artifact store reports when the run is cancelled during it. The run
/// then ends as cancelled; this text is never the error a user sees.
const STORE_WAIT_CANCELLED: &str = "run cancelled";

impl StoreSync {
    pub(crate) fn new(client: TransferClient, cancel: CancellationToken) -> Self {
        Self {
            layout: client.layout().clone(),
            client,
            fetchable: HashMap::new(),
            mismatched: HashMap::new(),
            missing: Vec::new(),
            present: None,
            cancel,
        }
    }

    /// Whether `path` is the local mirror of a cache hit that has not been
    /// fetched in this run: its artifact is read from the store if it is not
    /// here.
    pub(crate) fn holds(&self, path: &str) -> bool {
        self.fetchable.contains_key(path)
    }

    /// The local mirror paths of the artifacts [`Self::ensure_local`] found
    /// to be absent from the store since this was last called.
    pub(crate) fn take_missing(&mut self) -> Vec<String> {
        std::mem::take(&mut self.missing)
    }

    /// Whether a cache hit whose artifact is not on this disk is known to be
    /// absent, without asking a remote store: a local row, or a directory
    /// store, is a stat away. Unknown (false) for a result in a remote store.
    pub(crate) fn known_absent(store: Option<&Self>, path: &str) -> bool {
        match store.and_then(|s| s.fetchable.get(path)) {
            Some((_, at, _)) => {
                !std::path::Path::new(path).is_file()
                    && crate::transfer::local_path(at).is_some_and(|stored| !stored.exists())
            }
            None => !recover::on_disk(path, store.is_some()),
        }
    }

    /// Make sure the store itself is there before the cached results `lost`
    /// are computed again on the strength of "not in the store": its bucket,
    /// container or root directory must answer a listing. Asked once per run.
    ///
    /// An object that is absent from a store that is there is a missing
    /// artifact. A store that is gone, misnamed or unreachable answers the
    /// same way for every object, and recomputing then would turn an outage or
    /// a bad setting into a full recompute written to the wrong place.
    pub(crate) async fn confirm_present(&mut self, lost: &[String]) -> Result<(), String> {
        if self.present.is_none() {
            let answer = tokio::select! {
                biased;
                _ = self.cancel.cancelled() => return Err(STORE_WAIT_CANCELLED.to_string()),
                answer = self.client.probe() => answer,
            };
            self.present = Some(answer);
        }
        let Some(Err(why)) = &self.present else {
            return Ok(());
        };
        let shown: Vec<String> = lost.iter().take(10).map(|id| format!("  {id}")).collect();
        let more = match lost.len().saturating_sub(shown.len()) {
            0 => String::new(),
            n => format!("\n  ... and {n} more"),
        };
        Err(format!(
            "could not fetch {} cached artifact(s) from the artifact store: the store at {} \
             is not there or cannot be listed ({why}).\n{}{more}\n\
             Nothing was recomputed. Check the store location and credentials, or re-run with \
             --refresh-all to recompute them.",
            lost.len(),
            self.layout.store_root(),
            shown.join("\n")
        ))
    }

    /// Point lazily read parquet inputs that are not on this disk at the store,
    /// so the step's reader fetches only the byte ranges its query uses. They
    /// stay fetchable: a later eager reader still downloads the whole artifact.
    pub(crate) fn read_in_place(
        &self,
        provided: &mut HashMap<String, dispatch::ProvidedInput>,
        lazy: &HashSet<String>,
    ) {
        for (key, input) in provided.iter_mut() {
            let base = key.split_once('[').map_or(key.as_str(), |(b, _)| b);
            if !lazy.contains(key) && !lazy.contains(base) {
                continue;
            }
            let dispatch::ProvidedInput::Single(oref) = input else {
                continue;
            };
            if oref.format != "parquet" || std::path::Path::new(&oref.path).is_file() {
                continue;
            }
            if let Some((_, at, _)) = self.fetchable.get(&oref.path) {
                oref.path = at.clone();
            }
        }
    }

    /// Make the store-backed artifacts among `paths` local, reporting any
    /// fetch on stderr (through the progress bar when one is live). A fetch
    /// the store answers with "no such object" is not an error: its local
    /// path is kept for [`Self::take_missing`], and the caller decides
    /// whether to compute that result again. Any other failure (permissions,
    /// a store that cannot be reached) is an error naming what could not be
    /// fetched.
    pub(crate) async fn ensure_local<'a>(
        &mut self,
        paths: impl IntoIterator<Item = &'a str>,
        pb: Option<&indicatif::ProgressBar>,
    ) -> Result<(), String> {
        let mut locals = Vec::new();
        for path in paths {
            if let Some((node, store, sha256)) = self.fetchable.remove(path)
                && let Some(local) = self.client.fetch(&node, &store, sha256.as_deref())
            {
                locals.push(local);
            }
        }
        if locals.is_empty() {
            return Ok(());
        }
        let started = Instant::now();
        // Ctrl-C ends the wait at once: the run is cancelled and the helper is stopped, with
        // whatever it was downloading discarded (`TransferClient::abort`).
        let report = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err(STORE_WAIT_CANCELLED.to_string()),
            report = self.client.await_fetches(&locals) => report,
        };
        if report.transferred > 0 {
            let msg = format!(
                "[barca] fetched {} cached artifact{} ({}) in {:.1}s",
                report.transferred,
                if report.transferred == 1 { "" } else { "s" },
                fmt_bytes(report.bytes),
                started.elapsed().as_secs_f64()
            );
            match pb {
                Some(bar) if !bar.is_hidden() => bar.println(&msg),
                _ => eprintln!("{msg}"),
            }
        }
        // An artifact path is `{node}/{run_hash}`, so a refresh or another machine computing
        // the same step overwrites it; the store's copy is as valid a result as the recorded
        // one. Say so rather than fail.
        let mut differing: Vec<(String, &str, usize)> = Vec::new();
        for (node, at) in &report.mismatched {
            let base = crate::StepId::parse(node).base_id().to_string();
            match differing.iter_mut().find(|(b, _, _)| *b == base) {
                Some((_, _, count)) => *count += 1,
                None => differing.push((base, at, 1)),
            }
        }
        for (base, at, count) in differing {
            // The same words on stderr and, at the end of the run, in the JSON step entries.
            let finding = crate::mismatch::describe(&base, at, count - 1);
            let msg = format!("[barca] warning: {base}: {finding}");
            match pb {
                Some(bar) if !bar.is_hidden() => bar.println(&msg),
                _ => eprintln!("{msg}"),
            }
            self.mismatched.insert(base, finding);
        }
        let (missing, failed): (Vec<_>, Vec<_>) = report.failures.iter().partition(|f| f.missing);
        if failed.is_empty() {
            self.missing.extend(
                missing
                    .iter()
                    .filter_map(|f| self.layout.local_for(&f.store))
                    .map(|local| local.to_string_lossy().into_owned()),
            );
            return Ok(());
        }
        let detail: Vec<String> = failed
            .iter()
            .map(|f| format!("  {} ({}): {}", f.key, f.store, f.message))
            .collect();
        let messages: Vec<&str> = failed.iter().map(|f| f.message.as_str()).collect();
        Err(format!(
            "could not fetch {} cached artifact(s) from the artifact store:\n{}\n{}",
            failed.len(),
            detail.join("\n"),
            transfer_remedy(&messages, "Re-run with --refresh-all to recompute them.")
        ))
    }
}

/// What to do about failed transfers, given the helper's error messages; `otherwise` when
/// nothing more specific is known.
///
/// A directory at an object's path in a store that is a shared directory is not fixed by
/// recomputing: the upload would meet the same directory. Barca changes nothing in a store
/// but its own objects, so the directory has to be removed there. A local directory that
/// could not be moved aside says what to do in its own message.
pub(crate) fn transfer_remedy(messages: &[&str], otherwise: &str) -> String {
    if messages.iter().any(|m| m.starts_with("IsADirectoryError")) {
        "A directory sits where the artifact's object belongs in the store. Remove or rename \
         it there (barca changes nothing in a store but its own objects), then run the \
         command again."
            .to_string()
    } else if messages
        .iter()
        .any(|m| m.starts_with(BLOCKED_ARTIFACT_PATH))
    {
        "Then run the command again.".to_string()
    } else {
        otherwise.to_string()
    }
}
