//! History command implementation.

pub(crate) async fn history_cmd(
    env: Option<&str>,
    limit: Option<usize>,
    json: bool,
    fields: Option<&[String]>,
) -> Result<(), barca_core::BarcaError> {
    let cfg = barca_core::config::resolve(env)?;
    let (runs, total) = barca_core::queries::history(&cfg, limit).await?;
    super::emit(barca_core::report::render_history(
        &runs, total, json, fields,
    ));
    Ok(())
}
