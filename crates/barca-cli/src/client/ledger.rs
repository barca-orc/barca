//! The ledger of a local `get` / `run` ([`barca_core::ledger::LedgerTransport`] for
//! [`Server`]): `GET /health`, `POST /ledger/lookup`, `POST /ledger/runs`.

use super::{LEDGER_TIMEOUT, Server, transport_reason};
use barca_core::envelope::ErrorEnvelope;

/// The ledger requests of a local `get` / `run`: the JSON body of a 2xx, or why there is none.
/// What the answers mean is `barca_core::ledger`'s; this is only the transport.
impl Server {
    async fn ledger_call<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        route: &str,
    ) -> Result<T, barca_core::ledger::LedgerError> {
        use barca_core::ledger::LedgerError;
        let response = request
            .timeout(LEDGER_TIMEOUT)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| LedgerError::Unreachable(self.http.redact(&transport_reason(&e))))?;
        let status = response.status();
        let body = response
            .bytes()
            .await
            .map_err(|e| LedgerError::Unreachable(self.http.redact(&transport_reason(&e))))?;
        if status.is_success() {
            return serde_json::from_slice(&body).map_err(|e| {
                LedgerError::Unreachable(format!(
                    "{route} answered {status} with an unexpected body: {e}"
                ))
            });
        }
        let body = self.http.error_body(&body);
        match serde_json::from_slice::<ErrorEnvelope>(&body) {
            Ok(envelope) if status == reqwest::StatusCode::CONFLICT => {
                Err(LedgerError::Rejected(Box::new(envelope)))
            }
            Ok(envelope) => Err(LedgerError::Refused {
                status: status.as_u16(),
                envelope: Box::new(envelope),
            }),
            Err(_) => {
                let text = String::from_utf8_lossy(&body);
                let text: String = text
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(200)
                    .collect();
                Err(LedgerError::Unreachable(format!(
                    "{route} answered {status}: {text}"
                )))
            }
        }
    }

    fn ledger_post<'a, B: serde::Serialize + Sync, T: serde::de::DeserializeOwned + 'a>(
        &'a self,
        path: &'static str,
        body: &'a B,
    ) -> barca_core::ledger::BoxFuture<'a, Result<T, barca_core::ledger::LedgerError>> {
        Box::pin(async move {
            let body = serde_json::to_vec(body)
                .map_err(|e| barca_core::ledger::LedgerError::Unreachable(e.to_string()))?;
            let request = self
                .http
                .request(reqwest::Method::POST, path)
                .map_err(|e| barca_core::ledger::LedgerError::Unreachable(e.error))?
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body);
            self.ledger_call(request, &format!("POST {path}")).await
        })
    }
}

impl barca_core::ledger::LedgerTransport for Server {
    fn url(&self) -> &str {
        &self.url
    }

    fn health(
        &self,
    ) -> barca_core::ledger::BoxFuture<
        '_,
        Result<barca_core::ledger::ServerHealth, barca_core::ledger::LedgerError>,
    > {
        Box::pin(async move {
            let request = self
                .http
                .request(reqwest::Method::GET, "/health")
                .map_err(|e| barca_core::ledger::LedgerError::Unreachable(e.error))?;
            self.ledger_call(request, "GET /health").await
        })
    }

    fn lookup<'a>(
        &'a self,
        request: &'a barca_core::ledger::LookupRequest,
    ) -> barca_core::ledger::BoxFuture<
        'a,
        Result<barca_core::ledger::LookupResponse, barca_core::ledger::LedgerError>,
    > {
        self.ledger_post("/ledger/lookup", request)
    }

    fn post_run<'a>(
        &'a self,
        post: &'a barca_core::ledger::LedgerPost,
    ) -> barca_core::ledger::BoxFuture<
        'a,
        Result<barca_core::ledger::PostAccepted, barca_core::ledger::LedgerError>,
    > {
        self.ledger_post("/ledger/runs", post)
    }
}
