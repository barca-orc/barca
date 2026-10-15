//! History command implementation.

pub(crate) async fn history_cmd(
    env: Option<&str>,
    limit: Option<usize>,
    json: bool,
    fields: Option<&[String]>,
    reconcile: Option<&str>,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let (runs, total) = if let Some(run_id) = reconcile {
        (
            vec![barca_core::queries::reconcile_run(&cfg, run_id).await?],
            1,
        )
    } else {
        barca_core::queries::history(&cfg, limit).await?
    };
    super::emit(barca_core::report::render_history(
        &runs, total, json, fields,
    ));
    Ok(())
}
