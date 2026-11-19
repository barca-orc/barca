//! `barca gc` on a server (`POST /gc`): the eviction runs in the server, between its own
//! writes, and its result prints here as the local command prints its own.

use super::Server;
use crate::error::{CliError, ErrorKind};
use serde_json::json;
use std::time::Duration;

/// How long the server gets: it walks `.barca/` and may trim the metadata database first.
const GC_TIMEOUT: Duration = Duration::from_secs(30 * 60);

pub(crate) async fn gc(
    server: &Server,
    max_size: Option<&str>,
    dry_run: bool,
    json: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    // Checked here as locally; the server parses it again.
    if let Some(raw) = max_size {
        barca_core::gc::size::parse(raw).map_err(|e| {
            CliError::from_prose(ErrorKind::Usage, format!("error: --max-size: {e}"))
        })?;
    }
    server.health(env).await?;
    let what = "POST /gc";
    let request = server
        .http
        .request(reqwest::Method::POST, "/gc")?
        .timeout(GC_TIMEOUT)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(json!({"max_size": max_size, "dry_run": dry_run, "env": env}).to_string());
    let (_, body) = server.send(request, what).await?;
    let result: barca_core::gc::GcResult = serde_json::from_slice(&body)
        .map_err(|e| server.unexpected(what, &format!("not a gc result: {e}")))?;
    crate::commands::print_gc(&result, json);
    Ok(())
}
