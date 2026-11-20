//! Making an environment's metadata database usable, as one call: what every operation that
//! reads or writes it starts with.

/// Make this environment's metadata DB usable for reads and writes: its directories exist and
/// the database has barca's tables (a new project's first command creates it).
pub async fn open_metadata(
    cfg: &barca_model::config::ResolvedConfig,
) -> Result<(), barca_model::BarcaError> {
    barca_model::layout::ensure_env_dirs(&cfg.env)?;
    crate::db::init_db(&cfg.db_path).await
}
