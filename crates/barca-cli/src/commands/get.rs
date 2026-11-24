//! Get command implementation.

use super::execute::{cancel_on_ctrl_c, explain_cmd};
use crate::input::GetMode;
use barca_service::dispatch::OutputRef;
use barca_service::report::{GetShape, Returned, Shown, artifact_metadata};
use barca_service::selection::PartitionSelection;
use serde_json::Value;
use std::path::PathBuf;

/// A json value is previewed under its path in a terminal when its file is at most this big...
const PREVIEW_MAX_BYTES: u64 = 2048;
/// ...and it pretty-prints to at most this many lines.
const PREVIEW_MAX_LINES: usize = 10;

#[allow(clippy::too_many_arguments)]
pub(crate) async fn get_cmd(
    env: Option<&str>,
    targets: Vec<String>,
    files: Vec<PathBuf>,
    python: &std::path::Path,
    mode: GetMode,
    policy: barca_service::cache::CachePolicy,
    partition: Option<PartitionSelection>,
    batch: barca_service::batch::BatchOverride,
    dry_run: bool,
    agent: bool,
    fields: Option<&[String]>,
    ledger: Option<barca_service::ledger::Ledger>,
    allow_paused: bool,
) -> Result<(), barca_service::BarcaError> {
    let mut cfg = barca_service::config::resolve(env)?;
    if let Some(tier) = mode.cached_only {
        let request = barca_service::cached::Request {
            targets,
            files: files.iter().map(|p| p.display().to_string()).collect(),
            tier,
            partition: partition.map(|p| p.to_json().to_string()),
            values: mode.values,
            url_expires: mode.url_expires,
        };
        let answer = barca_service::cached::get(&cfg, python, &request).await?;
        return crate::print::print_cached(&answer, mode, fields);
    }
    // The configured server's ledger, unless --local (RFC-0007 section 4.1).
    cfg.ledger = ledger.map(barca_service::ledger::Ledger::attach);
    // The server backs up the ledger; this machine backs up nothing (barca docs server-mode).
    cfg.as_server_client();
    if mode.url_expires.is_some() {
        signable_store(&cfg)?;
        // A link is signed only for an object the store holds: the run checks the store and
        // repairs the targets in it, however recently it was checked (#593).
        cfg.check_store = true;
    }
    if let Some(line) = cfg.startup_line() {
        barca_service::errln!("{line}");
    }
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    if dry_run {
        return explain_cmd(
            &cfg,
            &targets,
            &file_args,
            python,
            policy,
            "get",
            mode.explain_mode(),
            fields,
            partition.as_ref(),
            &batch,
        )
        .await;
    }
    if !allow_paused {
        barca_service::pauses::check_request(&cfg.db_path, &targets).await?;
    }
    let (events, log_task) = super::execute::user_logs(mode.shape != GetShape::Json);
    let reader = Reader {
        python,
        cfg: &cfg,
        mode,
    };
    if targets.len() > 1 {
        let result = barca_service::commands::get_many_streaming(
            &cfg,
            &targets,
            &file_args,
            python,
            policy,
            agent,
            cancel_on_ctrl_c(),
            Some(events),
            partition,
            batch.clone(),
        )
        .await;
        let _ = log_task.await;
        let result = result?;
        let mut returned = Vec::with_capacity(result.targets.len());
        for (name, t) in &result.targets {
            returned.push(
                reader
                    .returned(
                        Some(name.as_str()),
                        t.final_output.as_ref(),
                        t.partitions.as_deref(),
                        t.key_summary.as_ref(),
                    )
                    .await?,
            );
        }
        super::emit(barca_service::report::render_get_multi(
            &result, &returned, mode.shape, fields,
        ));
        return match barca_service::report::first_multi_failure(&result) {
            Some(error) => Err(error),
            None => Ok(()),
        };
    }
    let target = targets.into_iter().next();
    let result = barca_service::commands::get_streaming(
        &cfg,
        target.as_deref(),
        &file_args,
        python,
        policy,
        agent,
        cancel_on_ctrl_c(),
        Some(events),
        partition,
        batch,
    )
    .await;
    let _ = log_task.await;
    let result = match result {
        Ok(result) => result,
        Err(e) => {
            // A partitioned target that stopped at a failed key still returns the keys that
            // succeeded; the failed ones are absent.
            let partial = match &e {
                barca_service::BarcaError::WorkerFailed(f) => {
                    match f.run.as_ref().and_then(|r| r.partitions.as_deref()) {
                        // A link that cannot be signed is left out: the run's own failure is
                        // what this command reports.
                        Some(keys) => reader
                            .returned(
                                target.as_deref(),
                                None,
                                Some(keys),
                                f.run.as_ref().and_then(|r| r.key_summary.as_ref()),
                            )
                            .await
                            .ok(),
                        None => None,
                    }
                }
                _ => None,
            };
            super::emit(barca_service::report::render_failed_get(
                &e,
                partial.as_ref(),
                mode.shape,
            ));
            return Err(e);
        }
    };
    let returned = reader
        .returned(
            target.as_deref(),
            result.final_output.as_ref(),
            result.partitions.as_deref(),
            result.key_summary.as_ref(),
        )
        .await?;
    super::emit(barca_service::report::render_get(
        &result,
        target.as_deref(),
        &returned,
        mode.shape,
        fields,
    ));
    Ok(())
}

/// Turns what a run returned into what the command prints.
struct Reader<'a> {
    python: &'a std::path::Path,
    cfg: &'a barca_service::config::ResolvedConfig,
    mode: GetMode,
}

impl Reader<'_> {
    /// One target's output: its artifact, or each of a partitioned target's keys. `target` is
    /// the name it was asked for (it names the node in a signed link's file name and errors).
    async fn returned(
        &self,
        target: Option<&str>,
        single: Option<&OutputRef>,
        keys: Option<&[(String, OutputRef)]>,
        summary: Option<&barca_service::results::KeySummary>,
    ) -> Result<Returned, barca_service::BarcaError> {
        let keys = barca_service::results::KeySummary::listed(summary, keys);
        let items: Vec<(Option<&str>, &OutputRef)> = match keys {
            Some(keys) => keys.iter().map(|(k, o)| (Some(k.as_str()), o)).collect(),
            None => single.into_iter().map(|o| (None, o)).collect(),
        };
        let refs: Vec<&OutputRef> = items.iter().map(|(_, o)| *o).collect();
        let signed = match self.mode.url_expires {
            Some(secs) => Some(self.sign(target, &items, secs).await?),
            None => None,
        };
        let shown_values: Vec<Value> = if let Some(signed) = &signed {
            refs.iter()
                .zip(signed)
                .map(|(o, s)| {
                    let mut pointer = artifact_metadata(o);
                    if let Some(Value::Object(inner)) = pointer.get_mut("_barca_artifact") {
                        inner.insert("url".into(), s.url.clone().into());
                        inner.insert("url_expires_at".into(), s.expires_at.clone().into());
                    }
                    pointer
                })
                .collect()
        } else if self.mode.values {
            barca_service::values::read_values(self.python, self.cfg, &refs)
                .await
                .into_iter()
                .zip(&refs)
                .map(|(value, oref)| {
                    value.unwrap_or_else(|e| {
                        barca_service::errln!(
                            "[barca] warning: could not read the value of {}: {e}; its path is \
                             returned instead",
                            oref.path
                        );
                        artifact_metadata(oref)
                    })
                })
                .collect()
        } else {
            refs.iter().map(|o| artifact_metadata(o)).collect()
        };
        let preview = self.mode.shape == GetShape::Pretty
            && !self.mode.values
            && signed.is_none()
            && std::io::IsTerminal::is_terminal(&std::io::stdout());
        let shown = items
            .iter()
            .enumerate()
            .map(|(i, (key, oref))| Shown {
                key: key.map(str::to_string),
                // Under -o url the link is printed where the path would be.
                path: match &signed {
                    Some(signed) => signed[i].url.clone(),
                    None => oref.path.clone(),
                },
                preview: preview.then(|| preview_of(oref)).flatten(),
            })
            .collect();
        let final_output = match keys {
            Some(_) => Some(barca_service::report::keyed_output(
                items
                    .iter()
                    .zip(shown_values)
                    .map(|((key, _), v)| (key.unwrap_or_default().to_string(), v))
                    .collect(),
                summary,
            )),
            None => shown_values.into_iter().next(),
        };
        Ok(Returned {
            final_output,
            values: self.mode.values,
            shown,
            key_summary: summary.map(barca_service::results::KeySummary::counts),
        })
    }

    /// A signed URL for each returned artifact's object in the store, in order. Any failure
    /// fails the command: exit 2 when signing cannot apply, else exit 3 naming the node.
    async fn sign(
        &self,
        target: Option<&str>,
        items: &[(Option<&str>, &OutputRef)],
        secs: u64,
    ) -> Result<Vec<barca_service::values::Signed>, barca_service::BarcaError> {
        use barca_service::BarcaError;
        let local_root = std::fs::canonicalize(&self.cfg.local_artifact_dir)
            .unwrap_or_else(|_| PathBuf::from(&self.cfg.local_artifact_dir));
        let layout =
            barca_service::transfer::ArtifactLayout::new(local_root, &self.cfg.artifact_root);
        let mut requests = Vec::with_capacity(items.len());
        let mut names = Vec::with_capacity(items.len());
        for (key, oref) in items {
            let node = node_name(target, &oref.path);
            let shown = match key {
                Some(k) => format!("'{node}' ({k})"),
                None => format!("'{node}'"),
            };
            let local = std::fs::canonicalize(&oref.path).unwrap_or_else(|_| {
                std::env::current_dir()
                    .map(|cwd| cwd.join(&oref.path))
                    .unwrap_or_else(|_| PathBuf::from(&oref.path))
            });
            let Some(uri) = layout.store_for(&local.to_string_lossy()) else {
                return Err(BarcaError::Other(format!(
                    "the result of {shown} ({}) is not under this environment's artifact \
                     directory, so it has no object in the store to sign\nRun `barca doctor` \
                     to check the store, then `barca get` again.",
                    oref.path
                )));
            };
            let ext = std::path::Path::new(&oref.path)
                .extension()
                .map(|e| format!(".{}", e.to_string_lossy()))
                .unwrap_or_default();
            requests.push(barca_service::values::SignRequest {
                uri,
                filename: format!("{node}{ext}"),
            });
            names.push(shown);
        }
        // A `local only` result (its upload failed after the step succeeded, #444) is uploaded
        // first, so it is signed rather than reported missing from the store.
        let stored: Vec<String> = requests.iter().map(|r| r.uri.clone()).collect();
        let interrupt = super::execute::cancel_on_ctrl_c();
        barca_service::commands::upload_pending_now(
            self.python,
            self.cfg,
            &stored,
            &interrupt.cancel,
        )
        .await?;
        let answers =
            barca_service::values::sign_urls(self.python, self.cfg, &requests, secs).await;
        let mut out = Vec::with_capacity(answers.len());
        for (answer, shown) in answers.into_iter().zip(&names) {
            match answer {
                Ok(signed) => {
                    if let Some(note) = &signed.note {
                        barca_service::errln!("[barca] warning: the link to {shown}: {note}");
                    }
                    out.push(signed);
                }
                Err(f) => return Err(sign_error(shown, &f)),
            }
        }
        Ok(out)
    }
}

/// The node a returned artifact belongs to, for a signed link's file name and for errors: the
/// target's name, else the artifact directory's (`pipeline.py--total` -> `total`).
fn node_name(target: Option<&str>, path: &str) -> String {
    if let Some(t) = target {
        return t.rsplit(':').next().unwrap_or(t).to_string();
    }
    let dir = std::path::Path::new(path)
        .parent()
        .and_then(|p| p.file_name())
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    match dir.rsplit_once("--") {
        Some((_, name)) if !name.is_empty() => name.to_string(),
        _ => dir,
    }
}

/// The error for one result that could not be signed: what, why, and what to run.
fn sign_error(shown: &str, f: &barca_service::values::SignFailure) -> barca_service::BarcaError {
    use barca_service::BarcaError;
    let fix = match f.kind.as_str() {
        "usage" => {
            return BarcaError::Usage(format!("could not sign a URL for {shown}: {}", f.message));
        }
        "missing" => {
            "The result is recorded but its object is not in the store. Run `barca \
                      doctor` to check the store, then refresh the node (`--refresh`) to compute \
                      and upload it again."
        }
        "credentials" => {
            "Run `barca doctor` to see which identity barca uses. Signing needs \
                          read access to the object and, on GCS without a key, \
                          roles/iam.serviceAccountTokenCreator; on Azure without an account \
                          key, permission to create user delegation keys (barca docs remote)."
        }
        _ => "Run `barca doctor` to check that the store is reachable, then retry.",
    };
    BarcaError::Other(format!(
        "could not sign a URL for {shown}: {}\n{fix}",
        f.message
    ))
}

/// `-o url` needs a cloud store: no store, a shared directory, `file://` or `memory://` have
/// no URL to share (exit 2).
fn signable_store(
    cfg: &barca_service::config::ResolvedConfig,
) -> Result<(), barca_service::BarcaError> {
    let root = &cfg.artifact_root;
    let scheme = root
        .split_once("://")
        .map(|(s, _)| s.to_ascii_lowercase())
        .unwrap_or_default();
    let cloud = matches!(
        scheme.as_str(),
        "s3" | "s3a" | "gs" | "gcs" | "abfs" | "abfss"
    );
    if cloud && cfg.remote_artifacts() {
        return Ok(());
    }
    let why = if !cfg.remote_artifacts() {
        format!("this project has no artifact store; its artifacts are local ({root})")
    } else if scheme.is_empty() || scheme == "file" {
        format!("the artifact store {root} is a directory, which has no URL to share")
    } else {
        format!("the artifact store {scheme}:// is not a cloud bucket")
    };
    Err(barca_service::BarcaError::Usage(format!(
        "-o url needs a cloud artifact store: {why}\nSet [remote] uri in barca.toml (or \
         BARCA_REMOTE_URI) to an s3://, gs:// or abfs:// store (barca docs remote), or use \
         -o path."
    )))
}

/// A small json value, pretty-printed, to show under its path.
fn preview_of(oref: &OutputRef) -> Option<String> {
    if oref.format != "json" || oref.size_bytes > PREVIEW_MAX_BYTES {
        return None;
    }
    let value: Value = serde_json::from_str(&std::fs::read_to_string(&oref.path).ok()?).ok()?;
    let text = serde_json::to_string_pretty(&value).ok()?;
    (text.lines().count() <= PREVIEW_MAX_LINES).then_some(text)
}
