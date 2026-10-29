//! Server-only credentials and request scope. General outgoing HTTP never receives them.
use crate::error::{CliError, Context, ErrorKind};
use barca_core::BarcaError;
use reqwest::{
    Method, Url,
    header::{AUTHORIZATION, HeaderValue},
};
use std::time::Duration;

pub(super) struct Http {
    client: reqwest::Client,
    base: Url,
    shown: String,
    raw: String,
    authorization: Option<HeaderValue>,
    secrets: Vec<String>,
}

fn usage(message: &str) -> CliError {
    CliError::from_barca(BarcaError::Usage(message.into()), &Context::default())
}

fn in_scope(base: &Url, url: &Url) -> bool {
    let prefix = base.path().trim_end_matches('/');
    let decoded = percent_encoding::percent_decode_str(url.path()).decode_utf8_lossy();
    let base_decoded = percent_encoding::percent_decode_str(base.path()).decode_utf8_lossy();
    let decoded_prefix = base_decoded.trim_end_matches('/');
    base.origin() == url.origin()
        && url.username().is_empty()
        && url.password().is_none()
        && !decoded
            .split('/')
            .any(|segment| matches!(segment, "." | ".."))
        && (decoded_prefix.is_empty()
            || decoded == decoded_prefix
            || decoded.starts_with(&format!("{decoded_prefix}/")))
        && (prefix.is_empty()
            || url.path() == prefix
            || url.path().starts_with(&format!("{prefix}/")))
}

impl Http {
    pub(super) fn new(
        raw: &str,
        token: Option<String>,
        timeout: Duration,
    ) -> Result<Self, CliError> {
        let mut base =
            Url::parse(raw).map_err(|_| usage("invalid server URL (host, port or proxy path)"))?;
        if !matches!(base.scheme(), "http" | "https")
            || base.host_str().is_none()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err(usage(
                "the server URL must be HTTP(S), with a host and optional proxy path, without a query or fragment",
            ));
        }
        let has_basic = !base.username().is_empty()
            || base.password().is_some()
            || raw.split_once("://").is_some_and(|(_, rest)| {
                rest.split('/')
                    .next()
                    .is_some_and(|authority| authority.contains('@'))
            });
        if has_basic && token.is_some() {
            return Err(usage(
                "BARCA_SERVER_TOKEN and server URL userinfo cannot be combined; choose bearer or Basic proxy credentials",
            ));
        }
        let mut secrets = Vec::new();
        let mut authorization = match token {
            Some(token) => {
                if token.is_empty()
                    || !token.is_ascii()
                    || token.chars().any(char::is_whitespace)
                    || token.chars().any(char::is_control)
                {
                    return Err(usage(
                        "BARCA_SERVER_TOKEN must be a nonempty ASCII token without whitespace or control characters",
                    ));
                }
                let header = HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| usage("BARCA_SERVER_TOKEN is not a valid authorization header"))?;
                secrets.push(token);
                Some(header)
            }
            None => None,
        };
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8()
                .map(|s| s.into_owned())
                .map_err(|_| usage("server URL userinfo must be valid percent-encoded UTF-8"))
        };
        let basic = if has_basic {
            let user = decode(base.username())?;
            let password = base.password().map(decode).transpose()?;
            if user.contains(':')
                || user.chars().any(char::is_control)
                || password
                    .as_deref()
                    .is_some_and(|p| p.chars().any(char::is_control))
            {
                return Err(usage(
                    "server Basic credentials contain an invalid username or control character",
                ));
            }
            if let Some(encoded) = base.password().filter(|p| !p.is_empty()) {
                secrets.push(encoded.into());
            }
            match password.as_deref().filter(|p| !p.is_empty()) {
                Some(password) => secrets.push(password.into()),
                None => secrets.push(user.clone()),
            }
            Some((user, password))
        } else {
            None
        };
        base.set_username("")
            .map_err(|_| usage("invalid server URL authority"))?;
        base.set_password(None)
            .map_err(|_| usage("invalid server URL authority"))?;
        let allowed = base.clone();
        let redirects = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 10 {
                attempt.error("too many server redirects")
            } else if in_scope(&allowed, attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("server redirect left the configured origin or proxy path")
            }
        });
        let client = barca_core::http::client_with_redirects(timeout, redirects)
            .map_err(|e| CliError::from_prose(ErrorKind::Infra, e))?;
        if let Some((user, password)) = basic {
            // Let reqwest own Basic encoding, then reuse its sensitive header on every
            // server request. No credential-bearing URL reaches the HTTP implementation.
            let request = client
                .get(base.clone())
                .basic_auth(user, password)
                .build()
                .map_err(|_| usage("invalid server Basic credentials"))?;
            authorization = request.headers().get(AUTHORIZATION).cloned();
        }
        if let Some(header) = authorization.as_mut() {
            header.set_sensitive(true);
            if let Ok(value) = header.to_str() {
                secrets.push(value.into());
                if let Some(encoded) = value.strip_prefix("Basic ") {
                    secrets.push(encoded.into());
                }
            }
        }
        secrets.retain(|s| !s.is_empty());
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Ok(Self {
            client,
            base,
            shown: barca_core::http::diagnostic_url(raw),
            raw: raw.into(),
            authorization,
            secrets,
        })
    }

    pub(super) fn shown_url(&self) -> &str {
        &self.shown
    }

    pub(super) fn request(
        &self,
        method: Method,
        route: &str,
    ) -> Result<reqwest::RequestBuilder, CliError> {
        let endpoint = format!("{}{route}", self.base.as_str().trim_end_matches('/'));
        let url = Url::parse(&endpoint)
            .ok()
            .filter(|url| route.starts_with('/') && in_scope(&self.base, url))
            .ok_or_else(|| {
                CliError::from_prose(
                    ErrorKind::Infra,
                    "server request left the configured origin or proxy path",
                )
            })?;
        let request = self.client.request(method, url);
        Ok(match &self.authorization {
            Some(header) => request.header(AUTHORIZATION, header.clone()),
            None => request,
        })
    }

    pub(super) fn redact(&self, text: &str) -> String {
        let mut text = text.replace(&self.raw, &self.shown);
        for secret in &self.secrets {
            text = text.replace(secret, "<redacted>");
        }
        text
    }

    /// Redact JSON string values before parsing an error envelope; raw replacement would
    /// miss secrets containing JSON escapes. Never use this on successful user data.
    pub(super) fn error_body(&self, body: &[u8]) -> Vec<u8> {
        fn visit(http: &Http, value: &mut serde_json::Value) {
            match value {
                serde_json::Value::String(s) => *s = http.redact(s),
                serde_json::Value::Array(items) => {
                    for item in items {
                        visit(http, item);
                    }
                }
                serde_json::Value::Object(items) => {
                    for item in items.values_mut() {
                        visit(http, item);
                    }
                }
                _ => {}
            }
        }
        match serde_json::from_slice::<serde_json::Value>(body) {
            Ok(mut value) => {
                visit(self, &mut value);
                serde_json::to_vec(&value).unwrap_or_default()
            }
            Err(_) => self.redact(&String::from_utf8_lossy(body)).into_bytes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(2);

    #[test]
    fn every_server_request_gets_the_sensitive_bearer_header_and_proxy_prefix() {
        let http = Http::new(
            "http://host/proxy",
            Some("private-token-value".into()),
            TIMEOUT,
        )
        .unwrap();
        for (method, route) in [
            (Method::GET, "/health"),
            (Method::GET, "/runs/id/events"),
            (Method::GET, "/history"),
            (Method::POST, "/runs"),
            (Method::DELETE, "/runs/id"),
            (Method::POST, "/ledger/lookup"),
            (Method::POST, "/ledger/runs"),
        ] {
            let request = http.request(method, route).unwrap().build().unwrap();
            let header = &request.headers()[AUTHORIZATION];
            assert_eq!(header, "Bearer private-token-value");
            assert!(header.is_sensitive());
            assert!(request.url().path().starts_with("/proxy/"));
            assert!(!format!("{request:?}").contains("private-token-value"));
        }
        let unrelated = barca_core::http::client(TIMEOUT)
            .unwrap()
            .get("http://host/other")
            .build()
            .unwrap();
        assert!(!unrelated.headers().contains_key(AUTHORIZATION));
    }

    #[test]
    fn percent_encoded_basic_credentials_are_headers_not_request_urls() {
        let http = Http::new("http://Alad%64in:open%20sesame@host/proxy", None, TIMEOUT).unwrap();
        let request = http
            .request(Method::GET, "/health")
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(request.url().as_str(), "http://host/proxy/health");
        assert_eq!(
            request.headers()[AUTHORIZATION],
            "Basic QWxhZGRpbjpvcGVuIHNlc2FtZQ=="
        );
        assert_eq!(http.shown_url(), "http://host/proxy");
        for secret in [
            "open sesame",
            "open%20sesame",
            "QWxhZGRpbjpvcGVuIHNlc2FtZQ==",
        ] {
            assert_eq!(http.redact(secret), "<redacted>");
        }
    }

    #[test]
    fn invalid_tokens_and_credential_conflicts_are_usage_errors_without_values() {
        for token in ["", " ", " private-value ", "private-value\n"] {
            let error = Http::new("http://host", Some(token.into()), TIMEOUT)
                .err()
                .unwrap();
            assert_eq!(error.kind, ErrorKind::Usage);
            assert!(
                !serde_json::to_string(&error)
                    .unwrap()
                    .contains("private-value")
            );
        }
        assert!(
            Http::new(
                "http://alice:password@host",
                Some("bearer-token".into()),
                TIMEOUT
            )
            .is_err()
        );
    }

    #[test]
    fn request_paths_cannot_escape_the_configured_project() {
        let http = Http::new("http://host/proxy", Some("secret".into()), TIMEOUT).unwrap();
        for route in ["/../outside", "/%2e%2e/outside", "missing-slash"] {
            assert!(http.request(Method::GET, route).is_err(), "{route}");
        }
        for url in [
            "http://other/proxy/health",
            "https://host/proxy/health",
            "http://host/proxy-two/health",
            "http://user@host/proxy/health",
        ] {
            assert!(!in_scope(&http.base, &Url::parse(url).unwrap()), "{url}");
        }
    }

    #[test]
    fn reflected_error_strings_are_redacted_even_when_json_escaped() {
        let token = "private\"token\\value";
        let http = Http::new("http://host/proxy", Some(token.into()), TIMEOUT).unwrap();
        let body =
            serde_json::to_vec(&serde_json::json!({"error":token,"nested":[{"traceback":token}]}))
                .unwrap();
        let sanitized: serde_json::Value = serde_json::from_slice(&http.error_body(&body)).unwrap();
        assert_eq!(sanitized["error"], "<redacted>");
        assert_eq!(sanitized["nested"][0]["traceback"], "<redacted>");
    }
}
