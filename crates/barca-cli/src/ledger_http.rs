//! The ledger transport of a local `get` / `run` against a server (RFC-0007 section 4.1):
//! `GET /health`, `POST /ledger/lookup` and `POST /ledger/runs` over plain HTTP/1.1, on the
//! hyper that `barca serve` already links. What the requests mean is `barca_core::ledger`'s.
//!
//! The `--server` client (#394) brings the CLI's HTTP client (TLS included) and the
//! resolution of `--server`, `BARCA_SERVER` and `--local`; [`attach`] and [`HttpLedger`] are
//! the two places it replaces.

use barca_core::envelope::ErrorEnvelope;
use barca_core::ledger::{
    BoxFuture, Ledger, LedgerError, LedgerPost, LedgerTransport, LookupRequest, LookupResponse,
    PostAccepted, ServerHealth,
};
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// Give `cfg` the ledger of its configured server (`[server] url`), if it has one.
pub(crate) fn attach(cfg: &mut barca_core::config::ResolvedConfig) {
    if let Some(url) = cfg.server_url.clone() {
        cfg.ledger = Some(Ledger::new(HttpLedger::new(url)));
    }
}

/// A server reached at `http://host[:port][/prefix]`.
pub(crate) struct HttpLedger {
    url: String,
}

impl HttpLedger {
    pub(crate) fn new(url: String) -> Self {
        Self { url }
    }

    /// `host:port` and the path prefix of the configured URL.
    fn target(&self) -> Result<(String, String), String> {
        let rest = match self.url.split_once("://") {
            Some(("http", rest)) => rest,
            Some((scheme, _)) => {
                return Err(format!(
                    "{scheme}:// is not supported yet for a local run's ledger; use an http:// \
                     URL (a TLS-terminating proxy in front of the server is fine)"
                ));
            }
            None => self.url.as_str(),
        };
        let (authority, prefix) = match rest.split_once('/') {
            Some((a, p)) => (a, format!("/{}", p.trim_end_matches('/'))),
            None => (rest, String::new()),
        };
        let authority = if authority.contains(':') {
            authority.to_string()
        } else {
            format!("{authority}:80")
        };
        let prefix = if prefix == "/" { String::new() } else { prefix };
        Ok((authority, prefix))
    }

    async fn call(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<(u16, Bytes), String> {
        let (authority, prefix) = self.target()?;
        let exchange = async {
            let stream = tokio::time::timeout(
                CONNECT_TIMEOUT,
                tokio::net::TcpStream::connect(authority.as_str()),
            )
            .await
            .map_err(|_| format!("no connection within {}s", CONNECT_TIMEOUT.as_secs()))?
            .map_err(|e| e.to_string())?;
            let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
                .await
                .map_err(|e| e.to_string())?;
            tokio::spawn(async move {
                let _ = conn.await;
            });
            let request = Request::builder()
                .method(method)
                .uri(format!("{prefix}{path}"))
                .header(hyper::header::HOST, authority.as_str())
                .header(hyper::header::CONTENT_TYPE, "application/json")
                .header(hyper::header::ACCEPT, "application/json")
                .body(Full::new(Bytes::from(body.unwrap_or_default())))
                .map_err(|e| e.to_string())?;
            let response = sender
                .send_request(request)
                .await
                .map_err(|e| e.to_string())?;
            let status = response.status().as_u16();
            let bytes = response
                .into_body()
                .collect()
                .await
                .map_err(|e| e.to_string())?
                .to_bytes();
            Ok::<_, String>((status, bytes))
        };
        tokio::time::timeout(REQUEST_TIMEOUT, exchange)
            .await
            .map_err(|_| format!("no answer within {}s", REQUEST_TIMEOUT.as_secs()))?
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> Result<T, LedgerError> {
        let (status, bytes) = self
            .call(method, path, body)
            .await
            .map_err(LedgerError::Unreachable)?;
        if (200..300).contains(&status) {
            return serde_json::from_slice(&bytes).map_err(|e| {
                LedgerError::Unreachable(format!(
                    "{path} answered {status} with an unexpected body: {e}"
                ))
            });
        }
        match serde_json::from_slice::<ErrorEnvelope>(&bytes) {
            Ok(envelope) if status == 409 => Err(LedgerError::Rejected(Box::new(envelope))),
            Ok(envelope) => Err(LedgerError::Refused {
                status,
                envelope: Box::new(envelope),
            }),
            Err(_) => Err(LedgerError::Unreachable(format!(
                "{path} answered {status}: {}",
                String::from_utf8_lossy(&bytes[..bytes.len().min(200)]).trim()
            ))),
        }
    }
}

impl LedgerTransport for HttpLedger {
    fn url(&self) -> &str {
        &self.url
    }

    fn health(&self) -> BoxFuture<'_, Result<ServerHealth, LedgerError>> {
        Box::pin(self.json(Method::GET, "/health", None))
    }

    fn lookup<'a>(
        &'a self,
        request: &'a LookupRequest,
    ) -> BoxFuture<'a, Result<LookupResponse, LedgerError>> {
        Box::pin(async move {
            let body =
                serde_json::to_vec(request).map_err(|e| LedgerError::Unreachable(e.to_string()))?;
            self.json(Method::POST, "/ledger/lookup", Some(body)).await
        })
    }

    fn post_run<'a>(
        &'a self,
        post: &'a LedgerPost,
    ) -> BoxFuture<'a, Result<PostAccepted, LedgerError>> {
        Box::pin(async move {
            let body =
                serde_json::to_vec(post).map_err(|e| LedgerError::Unreachable(e.to_string()))?;
            self.json(Method::POST, "/ledger/runs", Some(body)).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_split_into_authority_and_prefix() {
        let target = |url: &str| HttpLedger::new(url.to_string()).target();
        assert_eq!(
            target("http://127.0.0.1:8274").unwrap(),
            ("127.0.0.1:8274".to_string(), String::new())
        );
        assert_eq!(
            target("http://barca.internal/").unwrap(),
            ("barca.internal:80".to_string(), String::new())
        );
        assert_eq!(
            target("http://proxy:9000/barca/").unwrap(),
            ("proxy:9000".to_string(), "/barca".to_string())
        );
        assert!(
            target("https://barca.example.com")
                .unwrap_err()
                .contains("https://")
        );
    }
}
