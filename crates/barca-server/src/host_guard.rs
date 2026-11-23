//! Reject DNS-rebinding names before a loopback TCP request reaches any route.

use crate::{config::ServeConfig, error::ApiError, project_socket::ViaProjectSocket};
use axum::{
    extract::{Request, State},
    http::{header, uri::Authority},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::{net::IpAddr, sync::Arc};

fn allowed(value: &str, additional: &[String]) -> bool {
    if value.contains('@') {
        return false;
    }
    let Ok(authority) = value.parse::<Authority>() else {
        return false;
    };
    let suffix = &value[authority.host().len()..];
    if !suffix.is_empty()
        && !suffix
            .strip_prefix(':')
            .is_some_and(|port| !port.is_empty() && port.parse::<u16>().is_ok())
    {
        return false;
    }
    let host = authority
        .host()
        .strip_suffix('.')
        .unwrap_or(authority.host());
    let ip = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>();
    host.eq_ignore_ascii_case("localhost")
        || ip.is_ok_and(|ip| ip.is_loopback())
        || additional
            .iter()
            .any(|name| host.eq_ignore_ascii_case(name))
}

pub(crate) async fn validate(
    State(config): State<Arc<ServeConfig>>,
    request: Request,
    next: Next,
) -> Response {
    if !config.host.is_loopback() || request.extensions().get::<ViaProjectSocket>().is_some() {
        return next.run(request).await;
    }
    let mut hosts = request.headers().get_all(header::HOST).iter();
    let valid = match hosts.next() {
        Some(value) => {
            hosts.next().is_none()
                && value
                    .to_str()
                    .is_ok_and(|value| allowed(value, &config.resolved.server_allowed_hosts))
                && request.uri().authority().is_none_or(|value| {
                    allowed(value.as_str(), &config.resolved.server_allowed_hosts)
                })
        }
        // HTTP/2 carries the authority in the URI. Header-less HTTP/1.0 and in-process
        // requests have no browser-controlled host and preserve their existing behavior.
        None => request
            .uri()
            .authority()
            .is_none_or(|value| allowed(value.as_str(), &config.resolved.server_allowed_hosts)),
    };
    if !valid {
        return ApiError::Forbidden("Host is not allowed on this loopback server; use localhost or a loopback IP, or configure [server] allowed_hosts for a trusted reverse proxy".into()).into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::allowed;

    #[test]
    fn loopback_names_and_explicit_proxy_names_only() {
        let extra = vec!["barca.example.com".to_string()];
        for host in [
            "localhost",
            "LOCALHOST:8274",
            "localhost.:8274",
            "127.0.0.1:8274",
            "127.2.3.4",
            "[::1]:8274",
            "barca.example.com:443",
        ] {
            assert!(allowed(host, &extra), "{host}");
        }
        for host in [
            "evil.example",
            "localhost.evil.example",
            "evil.localhost",
            "0.0.0.0",
            "[::]",
            "localhost@evil.example",
            "evil@localhost",
            "localhost/path",
            "",
            "localhost,evil",
            "localhost..",
            "localhost:99999",
            "localhost:abc",
        ] {
            assert!(!allowed(host, &extra), "{host}");
        }
    }
}
