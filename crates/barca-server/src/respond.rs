//! Response bodies built from text the CLI's renderers produce.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// The stdout text of a rendering, without its final newline.
pub(crate) fn stdout_of(rendered: barca_service::report::RenderedOutput) -> String {
    let mut text: String = rendered
        .chunks
        .into_iter()
        .filter_map(|c| match c {
            barca_service::report::OutputChunk::Stdout(s) => Some(s),
            barca_service::report::OutputChunk::Stderr(_) => None,
        })
        .collect();
    if text.ends_with('\n') {
        text.pop();
    }
    text
}

/// A JSON body sent as it is (its keys in the order the CLI prints them).
pub(crate) fn json_text(status: StatusCode, text: String) -> Response {
    (
        status,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        text,
    )
        .into_response()
}
