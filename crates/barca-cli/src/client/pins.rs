//! `barca pin` and `barca unpin` with a server: the `/pins` and `/assets/{name}/pin` routes.

use super::{Server, encode};
use crate::commands::pin::{
    PinAction, render_list, render_pinned, render_unpinned, render_versions,
};
use crate::error::CliError;
use barca_core::pins::Choice;
use serde_json::{Value, json};
use std::path::PathBuf;

fn parse<T: serde::de::DeserializeOwned>(
    server: &Server,
    what: &str,
    body: Value,
) -> Result<T, CliError> {
    serde_json::from_value(body).map_err(|e| server.unexpected(what, &e.to_string()))
}

pub(crate) async fn pin(
    server: &Server,
    action: &PinAction,
    files: &[PathBuf],
    json: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    server.deployed_files(files).await?;
    match action {
        PinAction::List => {
            let body = server.get_json("/pins").await?;
            let pins: Vec<barca_core::db::AssetPin> =
                parse(server, "GET /pins", body["pins"].clone())?;
            render_list(&pins, json);
        }
        PinAction::Versions { target, limit } => {
            let path = format!("/assets/{}/versions?limit={limit}", encode(target));
            let body = server.get_json(&path).await?;
            let versions = parse(server, "GET /assets/{name}/versions", body)?;
            render_versions(&versions, json);
        }
        PinAction::Pin {
            target,
            choice,
            reason,
        } => {
            let mut body = json!({ "reason": reason });
            match choice {
                Choice::RunHash(hash) => body["run_hash"] = json!(hash),
                Choice::Previous => body["previous"] = json!(true),
            }
            let path = format!("/assets/{}/pin", encode(target));
            let text = server.post_json(&path, &body).await?;
            let value: Value = serde_json::from_str(&text)
                .map_err(|e| server.unexpected("POST /assets/{name}/pin", &e.to_string()))?;
            let pinned = parse(server, "POST /assets/{name}/pin", value)?;
            render_pinned(&pinned, json);
        }
        PinAction::Upload {
            target,
            path,
            reason,
        } => {
            let supplied = crate::commands::pin::read_supplied(path)?;
            let mut route = format!(
                "/assets/{}/pin/upload?file_name={}",
                encode(target),
                encode(&supplied.file_name)
            );
            if let Some(reason) = reason {
                route.push_str(&format!("&reason={}", encode(reason)));
            }
            let what = format!("POST /assets/{}/pin/upload", encode(target));
            let request = server
                .http
                .request(reqwest::Method::POST, &route)?
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                .body(supplied.bytes);
            let (_, body) = server.send(request, &what).await?;
            let value: Value = serde_json::from_slice(&body)
                .map_err(|e| server.unexpected(&what, &e.to_string()))?;
            let pinned = parse(server, &what, value)?;
            render_pinned(&pinned, json);
        }
    }
    Ok(())
}

pub(crate) async fn unpin(
    server: &Server,
    target: &str,
    files: &[PathBuf],
    json: bool,
    env: Option<&str>,
) -> Result<(), CliError> {
    server.health(env).await?;
    server.deployed_files(files).await?;
    let path = format!("/assets/{}/pin", encode(target));
    let what = format!("DELETE {path}");
    let request = server.http.request(reqwest::Method::DELETE, &path)?;
    let (_, body) = server.send(request, &what).await?;
    let value: Value =
        serde_json::from_slice(&body).map_err(|e| server.unexpected(&what, &e.to_string()))?;
    let unpinned = parse(server, &what, value)?;
    render_unpinned(&unpinned, json);
    Ok(())
}
