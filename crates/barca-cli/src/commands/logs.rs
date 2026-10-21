//! Local recorded stdout, independently of progress and result rendering.
/// Without a run id, reads the most recent run and names it on stderr.
pub(crate) async fn logs_cmd(
    env: Option<&str>,
    run_id: Option<&str>,
    step: Option<&str>,
    json: bool,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let run_id = match run_id {
        Some(id) => id.to_string(),
        None => {
            let run = barca_core::queries::latest_run(&cfg).await?;
            barca_core::errln!(
                "barca: logs of the most recent run: {} ({} {}, {}, started {})",
                run.run_id,
                run.command,
                barca_core::report::history_target(&run),
                run.status,
                barca_core::report::utc_cell(&run.started_at)
            );
            run.run_id
        }
    };
    let lines = barca_core::queries::logs(&cfg, &run_id, step).await?;
    if json {
        let entries: Vec<_> = lines
            .iter()
            .map(|entry| serde_json::json!({"node_id": entry.node_id, "line": entry.line}))
            .collect();
        barca_core::outln!(
            "{}",
            serde_json::to_string(&entries)
                .map_err(|e| barca_core::BarcaError::Other(e.to_string()))?
        );
    } else {
        for entry in lines {
            barca_core::outln!("{}", entry.line);
        }
    }
    Ok(())
}
