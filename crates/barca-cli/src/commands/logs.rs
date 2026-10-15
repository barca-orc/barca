//! Local recorded stdout, independently of progress and result rendering.
pub(crate) async fn logs_cmd(
    env: Option<&str>,
    run_id: &str,
    step: Option<&str>,
    json: bool,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let lines = barca_core::queries::logs(&cfg, run_id, step).await?;
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
