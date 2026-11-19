//! `barca gc`: evict cached results to keep `.barca/` under its size limit (RFC-0012).

use crate::error::{CliError, ErrorKind};
use barca_core::gc::GcOptions;

pub(crate) async fn gc_cmd(
    env: Option<&str>,
    max_size: Option<&str>,
    dry_run: bool,
    json: bool,
) -> Result<(), CliError> {
    let engine = |e: barca_core::BarcaError| CliError::from_barca(e, &Default::default());
    let max_size = max_size
        .map(barca_core::gc::size::parse)
        .transpose()
        .map_err(|e| CliError::from_prose(ErrorKind::Usage, format!("error: --max-size: {e}")))?;
    let cfg = barca_core::config::resolve(env).map_err(engine)?;
    let opts = GcOptions {
        max_size,
        dry_run,
        env: env.map(String::from),
    };
    let result = barca_core::gc::gc(&cfg, &opts).await.map_err(engine)?;
    crate::print::print_gc(&result, json);
    Ok(())
}
