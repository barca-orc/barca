//! A run's connection to a remote store: queue each finished artifact for upload, fetch remote
//! inputs before a phase reads them, and wait for the transfers at the end of the run.

use crate::text::human;
use crate::transfer::{ArtifactLayout, TransferClient};
use crate::verified_copies;
use barca_model::step_result::ProvidedInput;
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// How an error starts when a directory at an artifact path could not be moved aside
/// (`barca._storage.ArtifactPathError`): the state of barca's own artifact directory, not a
/// fault of the step that was writing there.
pub const BLOCKED_ARTIFACT_PATH: &str = "ArtifactPathError";

/// Whether the artifact recorded at `path` can be read from this machine as it is.
///
/// A file path (plain or `file://`) is looked up on disk. A remote URI cannot be: with a
/// separate artifact store (`separate_store`) workers read only the local artifact directory,
/// so a URI that is not in that store is out of reach; without one, the worker reads the URI
/// itself and reports what it finds, so it is taken as readable.
///
/// An artifact is one file. A directory at the path, or a symlink that leads to one or to
/// nothing, is not an artifact and counts as missing: the step is computed again, and whoever
/// writes the file moves the directory out of the way first (`barca._storage.make_way`).
pub fn on_disk(path: &str, separate_store: bool) -> bool {
    match crate::transfer::local_path(path) {
        Some(file) => file.is_file(),
        None => !separate_store,
    }
}

/// A store copy that is not the one its result was recorded with ([`barca_model::mismatch`] puts it
/// on the step reports), in words, without the `[barca] warning: <step>:` prefix of the stderr line:
/// `at` is the store location, `more` the number of further partitions of the step it holds
/// for.
pub fn describe_mismatch(base: &str, at: &str, more: usize) -> String {
    let others = match more {
        0 => String::new(),
        n => format!(" (and {n} more of its partitions)"),
    };
    format!(
        "the copy at {at}{others} is not the one this result was recorded with (another run \
         overwrote it, or it was changed). Using it. Recompute with --refresh {base}."
    )
}

/// This run's link to a separate artifact store: the transfer helper plus
/// the cache hits whose artifacts still live only in the store.
pub struct StoreSync {
    pub client: TransferClient,
    pub layout: ArtifactLayout,
    /// Local mirror path → (node id, store location, recorded SHA-256), for
    /// store-backed cache hits. Fetched on first use, so fully-cached
    /// intermediates a run never reads are never downloaded. With a recorded
    /// hash, a copy already on disk is checked against it on first use too, unless this
    /// process found it to hold that hash and it has not changed since
    /// ([`crate::verified_copies`]).
    pub fetchable: HashMap<String, (String, String, Option<String>)>,
    /// Base step id -> what was found, for each store copy fetched in this run that does not
    /// have its recorded hash. Put on the step reports when the run ends ([`barca_model::mismatch`]).
    pub mismatched: HashMap<String, String>,
    /// Store location -> SHA-256 of the store's copy, for each of those fetches: the copy now in
    /// use, recorded as the result's digest when the run ends ([`barca_db::db::adopt_store_digests`]).
    pub adopted: Vec<(String, String)>,
    /// Local mirror paths of fetches the store answered with "no such object",
    /// until [`Self::take_missing`] collects them.
    missing: Vec<String>,
    /// Whether the store itself is there, once it has been asked (see
    /// [`Self::confirm_present`]).
    present: Option<Result<(), String>>,
    /// The run's cancellation: a wait on the store ends when it fires.
    cancel: CancellationToken,
    /// Display id -> (store location, digest) of the object this machine knows the store holds
    /// for the run hash the sensor runs at ([`barca_db::db::confirmed_object`]), for each sensor
    /// decided to run. A poll that produces the same bytes there is not uploaded again (#554).
    pub confirmed: HashMap<String, (String, String)>,
    /// Sensor polls that preserved their observation and whose bytes the store already held.
    pub unchanged_observations: usize,
    /// Set when the run skipped the startup check (the store was checked in the last 5 minutes,
    /// #593): the check's time limit. The first fetch makes the check before it starts.
    check_before_fetching: Option<std::time::Duration>,
}

/// What to check when the artifact store fails its check (after "Nothing ran." at startup).
#[doc(hidden)]
#[macro_export]
macro_rules! store_check_fix {
    () => {
        "Check the store's URI (BARCA_REMOTE_URI or [remote] uri), that the bucket or container \
         exists (barca does not create it), the endpoint (FSSPEC_S3_ENDPOINT_URL for MinIO or \
         R2) and the credentials. BARCA_REMOTE=off runs one command without the store (barca \
         docs remote)."
    };
}
pub use store_check_fix;

/// What a wait on the artifact store reports when the run is cancelled during it. The run
/// then ends as cancelled; this text is never the error a user sees.
const STORE_WAIT_CANCELLED: &str = "run cancelled";

impl StoreSync {
    pub fn new(client: TransferClient, cancel: CancellationToken) -> Self {
        Self {
            layout: client.layout().clone(),
            client,
            fetchable: HashMap::new(),
            mismatched: HashMap::new(),
            adopted: Vec::new(),
            missing: Vec::new(),
            present: None,
            cancel,
            confirmed: HashMap::new(),
            unchanged_observations: 0,
            check_before_fetching: None,
        }
    }

    /// For a run that skipped the startup check (#593): check the store, within `limit`, before
    /// the first fetch, so a store that is gone or misconfigured is reported as such and not
    /// as missing objects to compute again. Uploads are not held back: a failed one is kept
    /// pending as usual.
    pub fn check_before_fetching(mut self, limit: std::time::Duration) -> Self {
        self.check_before_fetching = Some(limit);
        self
    }

    /// The startup check, made now (see [`Self::check_before_fetching`]); once per run.
    async fn check_now(&mut self) -> Result<(), String> {
        let Some(limit) = self.check_before_fetching.take() else {
            return Ok(());
        };
        let location = crate::transfer::diagnostic_uri(self.layout.store_root());
        barca_model::run_line!(
            Always,
            "[barca] checking artifact store {location} (timeout {}s)",
            limit.as_secs()
        );
        let answer = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => return Err(STORE_WAIT_CANCELLED.to_string()),
            answer = tokio::time::timeout(limit, self.client.preflight()) => answer,
        };
        match answer {
            Ok(Ok(())) => Ok(()),
            Ok(Err(why)) => Err(format!(
                "artifact store preflight at {location} failed: {why}\n{}",
                store_check_fix!()
            )),
            Err(_) => Err(format!(
                "artifact store preflight at {location} timed out after {}s\n{}",
                limit.as_secs(),
                store_check_fix!()
            )),
        }
    }

    /// Whether the store already holds `digest` at `store` for sensor `node_id`, by this
    /// machine's record of a confirmed upload: the poll's result need not be uploaded again.
    pub fn holds_already(&self, node_id: &str, store: &str, digest: &str) -> bool {
        self.confirmed
            .get(node_id)
            .is_some_and(|(at, known)| at == store && known == digest)
    }

    /// A newly written artifact supersedes a pending cache read at that mirror path.
    /// Never fetch an earlier sensor observation over the producer's new bytes.
    pub fn produced(&mut self, path: &str) {
        self.fetchable.remove(path);
        verified_copies::forget(std::path::Path::new(path));
    }

    /// Remember the local copies the helper just checked (`checking`: store location, local
    /// path, recorded digest) with the digest each now holds: the recorded one, or for a store
    /// copy that differs, the store copy's. A failed fetch leaves nothing remembered.
    fn remember_checked(
        &self,
        checking: &[(String, std::path::PathBuf, String)],
        report: &crate::transfer::TransferReport,
        hashed_from: std::time::SystemTime,
    ) {
        let failed: HashSet<&str> = report.failures.iter().map(|f| f.store.as_str()).collect();
        let differing: HashMap<&str, Option<&str>> = report
            .mismatched
            .iter()
            .map(|(_, at, sha256)| (at.as_str(), sha256.as_deref()))
            .collect();
        let mut good = Vec::new();
        for (store, local, recorded) in checking {
            let holds = match differing.get(store.as_str()) {
                _ if failed.contains(store.as_str()) => None,
                Some(store_copy) => *store_copy,
                None => Some(recorded.as_str()),
            };
            match holds {
                Some(sha256) => good.push((local.as_path(), sha256)),
                None => verified_copies::forget(local),
            }
        }
        verified_copies::remember(good, hashed_from);
    }

    /// Whether `path` is the local mirror of a cache hit that has not been
    /// fetched in this run: its artifact is read from the store if it is not
    /// here.
    pub fn holds(&self, path: &str) -> bool {
        self.fetchable.contains_key(path)
    }

    /// The local mirror paths of the artifacts [`Self::ensure_local`] found
    /// to be absent from the store since this was last called.
    pub fn take_missing(&mut self) -> Vec<String> {
        std::mem::take(&mut self.missing)
    }

    /// Whether a cache hit whose artifact is not on this disk is known to be
    /// absent, without asking a remote store: a local row, or a directory
    /// store, is a stat away. Unknown (false) for a result in a remote store.
    pub fn known_absent(store: Option<&Self>, path: &str) -> bool {
        match store.and_then(|s| s.fetchable.get(path)) {
            Some((_, at, _)) => {
                !std::path::Path::new(path).is_file()
                    && crate::transfer::local_path(at).is_some_and(|stored| !stored.exists())
            }
            None => !on_disk(path, store.is_some()),
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
    pub async fn confirm_present(&mut self, lost: &[String]) -> Result<(), String> {
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
    pub fn read_in_place(
        &self,
        provided: &mut HashMap<String, ProvidedInput>,
        lazy: &HashSet<String>,
    ) {
        for (key, input) in provided.iter_mut() {
            let base = key.split_once('[').map_or(key.as_str(), |(b, _)| b);
            if !lazy.contains(key) && !lazy.contains(base) {
                continue;
            }
            let ProvidedInput::Single(oref) = input else {
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
    pub async fn ensure_local<'a>(
        &mut self,
        paths: impl IntoIterator<Item = &'a str>,
        pb: Option<&indicatif::ProgressBar>,
    ) -> Result<(), String> {
        let began = Instant::now();
        let mut wanted = Vec::new();
        for path in paths {
            if let Some(entry) = self.fetchable.remove(path) {
                wanted.push(entry);
            }
        }
        let asked = wanted.len();
        // A local copy this process already found to hold the recorded digest, unchanged
        // since, is not hashed again (#1010, `crate::verified_copies`). The rest are checked
        // as before; what those checks find is remembered below.
        let hashed_from = std::time::SystemTime::now();
        wanted.retain(
            |(_, store, sha256)| match (sha256, self.layout.local_for(store)) {
                (Some(sha256), Some(local)) => !verified_copies::holds(&local, sha256),
                _ => true,
            },
        );
        let remembered = asked - wanted.len();
        // A helper that was not started (the store was checked in the last 5 minutes, #593) is
        // started only if a local copy is missing or is not the recorded one; the store is
        // checked first.
        let mut hashed_here = 0;
        if !self.client.started() {
            let mut good = Vec::new();
            wanted.retain(|(_, store, sha256)| {
                let here = self.client.has_local_copy(store, sha256.as_deref());
                if let (true, Some(sha256)) = (here, sha256) {
                    good.extend(self.layout.local_for(store).map(|l| (l, sha256.clone())));
                }
                hashed_here += usize::from(sha256.is_some());
                !here
            });
            verified_copies::remember(
                good.iter().map(|(l, h)| (l.as_path(), h.as_str())),
                hashed_from,
            );
            if !wanted.is_empty() {
                self.check_now().await?;
            }
        }
        let mut locals = Vec::new();
        let mut checking = Vec::new();
        for (node, store, sha256) in wanted {
            if let Some(local) = self.client.fetch(&node, &store, sha256.as_deref()) {
                if let Some(sha256) = sha256 {
                    checking.push((store, local.clone(), sha256));
                }
                locals.push(local);
            }
        }
        let trace = |sent: usize| {
            if asked > 0 && std::env::var_os("BARCA_TRACE_TIMING").is_some() {
                barca_model::errln!(
                    "[trace]  ensure_local: {asked} cached input(s), {remembered} unchanged since \
                     checked, {hashed_here} hashed here, {sent} sent to the transfer helper to check, \
                     {:.1}ms",
                    began.elapsed().as_secs_f64() * 1000.0
                );
            }
        };
        if locals.is_empty() {
            trace(0);
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
        trace(checking.len());
        self.remember_checked(&checking, &report, hashed_from);
        if report.transferred > 0 {
            let msg = format!(
                "[barca] fetched {} cached artifact{} ({}) in {:.1}s",
                report.transferred,
                if report.transferred == 1 { "" } else { "s" },
                human(report.bytes),
                started.elapsed().as_secs_f64()
            );
            barca_model::lines::Lines::current().note(pb, &msg);
        }
        // An artifact path is `{node}/{run_hash}`, so a refresh or another machine computing
        // the same step overwrites it; the store's copy is as valid a result as the recorded
        // one. Say so rather than fail.
        let mut differing: Vec<(String, &str, usize)> = Vec::new();
        for (node, at, sha256) in &report.mismatched {
            if let Some(sha256) = sha256 {
                self.adopted.push((at.clone(), sha256.clone()));
            }
            let base = barca_model::StepId::parse(node).base_id().to_string();
            match differing.iter_mut().find(|(b, _, _)| *b == base) {
                Some((_, _, count)) => *count += 1,
                None => differing.push((base, at, 1)),
            }
        }
        for (base, at, count) in differing {
            // The same words on stderr and, at the end of the run, in the JSON step entries.
            let finding = describe_mismatch(&base, at, count - 1);
            let msg = format!("[barca] warning: {base}: {finding}");
            barca_model::lines::Lines::current().note(pb, &msg);
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
pub fn transfer_remedy(messages: &[&str], otherwise: &str) -> String {
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
