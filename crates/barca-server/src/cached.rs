//! A cached read has no access to the run queue or execution state.
use crate::{error::ApiError, read_cache::Reads};
use axum::{Json, extract::State};
use barca_service::cached::{Answer, Request};

pub async fn get(
    State(reads): State<Reads>,
    Json(mut request): Json<Request>,
) -> Result<(axum::http::StatusCode, Json<Answer>), ApiError> {
    if request.files.is_empty() {
        request.files = reads.config.files.clone();
    } else if request
        .files
        .iter()
        .any(|f| !reads.config.files.contains(f))
    {
        return Err(ApiError::BadRequest(
            "cached read files must belong to this deployment".into(),
        ));
    }
    let _permit = reads
        .artifact_reads
        .acquire()
        .await
        .expect("read capacity stays open");
    let mut answer =
        barca_service::cached::get(&reads.config.resolved, &reads.config.python, &request).await?;
    answer.path_scope = "server".into();
    let status = if answer.error.is_some() {
        axum::http::StatusCode::NOT_FOUND
    } else {
        axum::http::StatusCode::OK
    };
    Ok((status, Json(answer)))
}
