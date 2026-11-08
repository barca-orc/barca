//! `barca pin` and `barca unpin`: hold an asset at an earlier materialization (`barca docs pins`).

use crate::error::{CliError, ErrorKind};
use barca_core::db::{AssetPin, Version};
use barca_core::pins::{Choice, Pinned, Unpinned, Versions};
use std::path::PathBuf;

/// What one `barca pin` invocation asks for.
#[derive(Debug, Clone)]
pub(crate) enum PinAction {
    /// No target: every pin.
    List,
    Versions {
        target: String,
        limit: usize,
    },
    Pin {
        target: String,
        choice: Choice,
        reason: Option<String>,
    },
    /// Pin to data the operator supplies (`--from-file`).
    Upload {
        target: String,
        path: PathBuf,
        reason: Option<String>,
    },
}

/// The action the flags of `barca pin` name; a target without one is a usage error.
pub(crate) fn action(
    target: Option<String>,
    run_hash: Option<String>,
    previous: bool,
    from_file: Option<PathBuf>,
    versions: bool,
    limit: Option<usize>,
    reason: Option<String>,
) -> Result<PinAction, CliError> {
    let usage = |text: &str| CliError::from_prose(ErrorKind::Usage, text.to_string());
    let Some(target) = target else {
        if run_hash.is_some() || previous || from_file.is_some() || versions || reason.is_some() {
            return Err(usage(
                "error: name the asset to pin: barca pin <TARGET> --previous\n\
                 Without a target, `barca pin` lists every pin.",
            ));
        }
        return Ok(PinAction::List);
    };
    if versions {
        return Ok(PinAction::Versions {
            target,
            limit: limit.unwrap_or(barca_core::pins::DEFAULT_VERSIONS),
        });
    }
    if let Some(path) = from_file {
        return Ok(PinAction::Upload {
            target,
            path,
            reason,
        });
    }
    let choice = match (run_hash, previous) {
        (Some(hash), _) => Choice::RunHash(hash),
        (None, true) => Choice::Previous,
        (None, false) => {
            return Err(usage(&format!(
                "error: say which materialization to pin: --run-hash <HASH>, --previous or \
                 --from-file <PATH>\n\
                 List them with: barca pin {target} --versions"
            )));
        }
    };
    Ok(PinAction::Pin {
        target,
        choice,
        reason,
    })
}

fn engine(e: barca_core::BarcaError) -> CliError {
    CliError::from_barca(e, &Default::default())
}

pub(crate) async fn pin_cmd(
    env: Option<&str>,
    action: &PinAction,
    files: Vec<PathBuf>,
    json: bool,
    python: &std::path::Path,
) -> Result<(), CliError> {
    let cfg = barca_core::config::resolve(env).map_err(engine)?;
    if let PinAction::List = action {
        let pins = barca_core::pins::list(&cfg.db_path).await.map_err(engine)?;
        render_list(&pins, json);
        return Ok(());
    }
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let dag = barca_core::load::build_dag(&file_args, python)
        .await
        .map_err(engine)?;
    match action {
        PinAction::List => unreachable!(),
        PinAction::Versions { target, limit } => {
            let versions = barca_core::pins::versions(&cfg.db_path, &dag, target, *limit)
                .await
                .map_err(engine)?;
            render_versions(&versions, json);
        }
        PinAction::Pin {
            target,
            choice,
            reason,
        } => {
            barca_core::layout::ensure_env_dirs(&cfg.env).map_err(engine)?;
            barca_core::db::init_db(&cfg.db_path)
                .await
                .map_err(engine)?;
            let pinned = barca_core::pins::pin(
                &cfg.db_path,
                &dag,
                target,
                choice,
                reason.clone(),
                barca_core::pins::local_actor(),
            )
            .await
            .map_err(engine)?;
            render_pinned(&pinned, json);
        }
        PinAction::Upload {
            target,
            path,
            reason,
        } => {
            let supplied = read_supplied(path)?;
            barca_core::layout::ensure_env_dirs(&cfg.env).map_err(engine)?;
            barca_core::db::init_db(&cfg.db_path)
                .await
                .map_err(engine)?;
            let pinned = barca_core::pins::pin_upload(
                &cfg,
                python,
                &dag,
                target,
                supplied,
                reason.clone(),
                barca_core::pins::local_actor(),
            )
            .await
            .map_err(engine)?;
            render_pinned(&pinned, json);
        }
    }
    Ok(())
}

/// The file `--from-file` names, read whole (refused over the upload limit before reading).
pub(crate) fn read_supplied(
    path: &std::path::Path,
) -> Result<barca_core::pins::Supplied, CliError> {
    let usage = |text: String| CliError::from_prose(ErrorKind::Usage, text);
    let meta = std::fs::metadata(path).map_err(|e| {
        usage(format!(
            "error: cannot read --from-file {}: {e}",
            path.display()
        ))
    })?;
    if !meta.is_file() {
        return Err(usage(format!(
            "error: --from-file {} is not a file",
            path.display()
        )));
    }
    if meta.len() > barca_core::pins::MAX_UPLOAD_BYTES as u64 {
        return Err(usage(format!(
            "error: --from-file {} is {} bytes, more than the {} an upload may be",
            path.display(),
            meta.len(),
            barca_core::gc::size::human(barca_core::pins::MAX_UPLOAD_BYTES as u64)
        )));
    }
    let bytes = std::fs::read(path).map_err(|e| {
        usage(format!(
            "error: cannot read --from-file {}: {e}",
            path.display()
        ))
    })?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    barca_core::pins::upload_format(&file_name, None).map_err(|e| usage(format!("error: {e}")))?;
    Ok(barca_core::pins::Supplied {
        bytes,
        file_name,
        format: None,
    })
}

pub(crate) async fn unpin_cmd(
    env: Option<&str>,
    target: &str,
    files: Vec<PathBuf>,
    json: bool,
    python: &std::path::Path,
) -> Result<(), CliError> {
    let cfg = barca_core::config::resolve(env).map_err(engine)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let dag = barca_core::load::build_dag(&file_args, python)
        .await
        .map_err(engine)?;
    let unpinned = barca_core::pins::unpin(&cfg.db_path, &dag, target)
        .await
        .map_err(engine)?;
    render_unpinned(&unpinned, json);
    Ok(())
}

fn short(id: &str) -> &str {
    id.rsplit_once(':').map_or(id, |(_, name)| name)
}

fn hash(h: &str) -> &str {
    &h[..h.len().min(12)]
}

fn json_line(value: &impl serde::Serialize) {
    barca_core::outln!("{}", serde_json::to_string(value).unwrap_or_default());
}

fn by(pin: &AssetPin) -> String {
    let mut out = format!("since {}", pin.pinned_at);
    if let Some(by) = &pin.pinned_by {
        out.push_str(&format!(" by {by}"));
    }
    if let Some(reason) = &pin.reason {
        out.push_str(&format!(": {reason}"));
    }
    out
}

pub(crate) fn render_list(pins: &[AssetPin], json: bool) {
    if json {
        json_line(&serde_json::json!({ "pins": pins }));
        return;
    }
    if pins.is_empty() {
        barca_core::outln!("no pinned assets");
        return;
    }
    for pin in pins {
        barca_core::outln!(
            "{}  pinned at {}  {}",
            pin.node_id,
            hash(&pin.run_hash),
            by(pin)
        );
    }
}

fn size(v: &Version) -> String {
    v.size_bytes
        .map(|b| barca_core::gc::size::human(b.max(0) as u64))
        .unwrap_or_else(|| "-".into())
}

pub(crate) fn render_versions(v: &Versions, json: bool) {
    if json {
        json_line(v);
        return;
    }
    if let Some(pin) = &v.pin {
        barca_core::outln!(
            "{} is pinned at {} {}",
            short(&v.node_id),
            hash(&pin.run_hash),
            by(pin)
        );
    }
    if !v.pinnable {
        barca_core::outln!(
            "{} cannot be pinned (only an asset without partitions can)",
            short(&v.node_id)
        );
    }
    if v.versions.is_empty() {
        barca_core::outln!("no successful materializations of {}", short(&v.node_id));
        return;
    }
    barca_core::outln!(
        "{:<12}  {:<20}  {:>9}  {:>9}  {:<24}  note",
        "run hash",
        "materialized at",
        "duration",
        "size",
        "run id"
    );
    for ver in &v.versions {
        let mut note = Vec::new();
        if ver.current {
            note.push("latest");
        }
        if ver.pinned {
            note.push("pinned");
        }
        if !ver.available {
            note.push("evicted");
        }
        if ver.upload.is_some() {
            note.push("uploaded");
        }
        barca_core::outln!(
            "{:<12}  {:<20}  {:>9}  {:>9}  {:<24}  {}",
            hash(&ver.run_hash),
            ver.materialized_at,
            ver.elapsed_seconds
                .map(|s| format!("{s:.2}s"))
                .unwrap_or_else(|| "-".into()),
            size(ver),
            ver.run_id.as_deref().unwrap_or("-"),
            note.join(", ")
        );
    }
}

pub(crate) fn render_pinned(p: &Pinned, json: bool) {
    if json {
        json_line(p);
        return;
    }
    if let Some(up) = &p.version.upload {
        barca_core::outln!(
            "pinned {} at {}, uploaded from {} ({}); runs read it and never recompute it until \
             `barca unpin {}`",
            short(&p.node_id),
            hash(&p.pin.run_hash),
            up.file_name.as_deref().unwrap_or("a file"),
            size(&p.version),
            short(&p.node_id)
        );
        if let Some(old) = &p.replaced {
            barca_core::errln!("[barca] replaced its pin at {}", hash(&old.run_hash));
        }
        return;
    }
    barca_core::outln!(
        "pinned {} at {} (materialized {}, run {}); runs read it and never recompute it until \
         `barca unpin {}`",
        short(&p.node_id),
        hash(&p.pin.run_hash),
        p.version.materialized_at,
        p.version.run_id.as_deref().unwrap_or("-"),
        short(&p.node_id)
    );
    if let Some(old) = &p.replaced {
        barca_core::errln!("[barca] replaced its pin at {}", hash(&old.run_hash));
    }
}

pub(crate) fn render_unpinned(u: &Unpinned, json: bool) {
    if json {
        json_line(u);
        return;
    }
    match &u.removed {
        Some(pin) => barca_core::outln!(
            "unpinned {} (was pinned at {}); the next run decides it as before",
            short(&u.node_id),
            hash(&pin.run_hash)
        ),
        None => barca_core::outln!("{} was not pinned", short(&u.node_id)),
    }
}
