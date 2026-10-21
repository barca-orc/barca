//! `barca state backups`, `barca state restore` (RFC-0009) and `barca state reset`.

use barca_core::BarcaError;

pub(crate) async fn state_backups_cmd(
    env: Option<&str>,
    python: &std::path::Path,
    json: bool,
) -> Result<(), BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let listed = barca_core::backup::list(python, &cfg).await?;
    if let Some(why) = &listed.store_error {
        barca_core::errln!("[barca] warning: {why}; listing the local backups only");
    }
    if json {
        barca_core::outln!(
            "{}",
            serde_json::to_string(&listed).map_err(|e| BarcaError::Other(e.to_string()))?
        );
        return Ok(());
    }
    if listed.backups.is_empty() {
        barca_core::outln!(
            "no backups in {}{}",
            listed.local_dir,
            match &listed.store {
                Some(store) => format!(" or {store}"),
                None => " (no store is configured: backups are taken only with [remote] uri)"
                    .to_string(),
            }
        );
        return Ok(());
    }
    let rows: Vec<[String; 6]> = listed
        .backups
        .iter()
        .map(|b| {
            [
                b.index.to_string(),
                b.name.clone(),
                match b.agree {
                    Some(false) => format!("{} (differ)", b.location),
                    _ => b.location.clone(),
                },
                barca_core::report::fmt_bytes(b.size_bytes),
                b.rows.map_or_else(|| "-".to_string(), |r| r.to_string()),
                b.run_id.clone().unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect();
    let header = ["#", "NAME", "WHERE", "SIZE", "ROWS", "RUN"];
    let widths: Vec<usize> = (0..header.len())
        .map(|i| {
            rows.iter()
                .map(|r| r[i].chars().count())
                .chain([header[i].len()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let line = |cells: &[String]| {
        let mut out = String::new();
        for (i, c) in cells.iter().enumerate() {
            out.push_str(&format!("{c:<w$}  ", w = widths[i]));
        }
        barca_core::outln!("{}", out.trim_end());
    };
    line(&header.map(String::from));
    for r in &rows {
        line(r);
    }
    barca_core::outln!(
        "\nlocal: {}{}",
        listed.local_dir,
        listed
            .store
            .as_deref()
            .map(|s| format!("\nstore: {s}"))
            .unwrap_or_default()
    );
    Ok(())
}

pub(crate) async fn state_restore_cmd(
    env: Option<&str>,
    from: Option<&str>,
    python: &std::path::Path,
    json: bool,
) -> Result<(), BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let restored = barca_core::backup::restore(python, &cfg, from).await?;
    barca_core::errln!(
        "[barca] restored {} from the {} copy ({}){}; {}",
        restored.restored,
        restored.from,
        restored.source,
        match (restored.rows, &restored.run_id) {
            (Some(rows), Some(run)) => format!(", {rows} rows, taken by run {run}"),
            (Some(rows), None) => format!(", {rows} rows"),
            _ => String::new(),
        },
        match &restored.pre_restore {
            Some(pre) => format!("the database it replaced is {pre}"),
            None => "there was no database to replace".to_string(),
        }
    );
    if json {
        barca_core::outln!(
            "{}",
            serde_json::to_string(&restored).map_err(|e| BarcaError::Other(e.to_string()))?
        );
    }
    Ok(())
}

pub(crate) async fn state_reset_cmd(
    env: Option<&str>,
    yes: bool,
    json: bool,
) -> Result<(), BarcaError> {
    use std::io::IsTerminal;
    let cfg = barca_core::config::resolve(env)?;
    if let Some(holder) = barca_core::backup::ServeLock::holder(&cfg) {
        return Err(BarcaError::Other(format!(
            "cannot reset while barca serve is using this environment ({holder}). Nothing was \
             changed.\nUse the Reset database button in its web UI, or POST /state/reset to it; \
             or stop the server and run this command again."
        )));
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            return Err(BarcaError::Usage(
                "barca state reset asks before it moves the database aside, and stdin is not a \
                 terminal: pass --yes"
                    .to_string(),
            ));
        }
        barca_core::errln!(
            "This moves {} (and its -wal and -shm) aside to {}.reset-<UTC> and starts an empty \
             database: run history, cache records and schedule pauses start over. Nothing is \
             deleted; barca state restore --from <the moved file> undoes it.",
            cfg.db_path,
            cfg.db_path
        );
        barca_core::term::stderr_str("Reset the metadata database? [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes" | "YES") {
            barca_core::errln!("[barca] not reset: nothing was changed");
            return Ok(());
        }
    }
    let reset = barca_core::backup::reset(&cfg, |running| {
        if running.is_empty() {
            return Ok(());
        }
        Err(BarcaError::Other(format!(
            "cannot reset while a run is in progress ({}). Nothing was changed.\nWait for it \
             to finish, or cancel it (Ctrl-C in its terminal), then run this command again.",
            running.join(", ")
        )))
    })
    .await?;
    match (&reset.moved_to, &reset.undo) {
        (Some(moved), Some(undo)) => barca_core::errln!(
            "[barca] reset the metadata database: the old one is {moved}, {} is new and empty. \
             Undo: {undo}",
            reset.database
        ),
        _ => barca_core::errln!(
            "[barca] there was no metadata database; {} is new and empty",
            reset.database
        ),
    }
    if json {
        barca_core::outln!(
            "{}",
            serde_json::to_string(&reset).map_err(|e| BarcaError::Other(e.to_string()))?
        );
    }
    Ok(())
}
