//! barca's outgoing HTTPS client.
//!
//! Every outgoing HTTPS request goes through [`client`], so the TLS stack (rustls with the
//! `ring` provider, never native-tls) and its root certificates are set up in one place.
//! Roots come from the operating system's store, through `rustls-platform-verifier`, so a
//! corporate proxy's CA that the machine trusts is trusted here too.

use std::time::Duration;

/// A server URL for diagnostics: omit authority userinfo, preserving its host and proxy path.
/// This also handles malformed URLs, which must be safe to name in validation errors.
pub fn diagnostic_url(url: &str) -> String {
    let start = url.find("://").map_or(0, |i| i + 3);
    let end = url[start..]
        .find(['/', '?', '#'])
        .map_or(url.len(), |i| start + i);
    match url[start..end].rfind('@') {
        Some(at) => format!("{}{}", &url[..start], &url[start + at + 1..]),
        None => url.to_string(),
    }
}

/// A client for HTTPS requests, each bounded by `timeout` from connecting to the last byte of
/// the reply. Building one reads the system's root certificates, so build it once and reuse it.
///
/// The error says why no client could be built, without naming any URL.
pub fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    client_with_redirects(timeout, reqwest::redirect::Policy::default())
}

/// A caller-specific redirect policy with the same TLS/timeout setup. Credentials belong
/// to that caller, never to this general-purpose client module.
pub fn client_with_redirects(
    timeout: Duration,
    redirects: reqwest::redirect::Policy,
) -> Result<reqwest::Client, String> {
    // reqwest is built without a crypto provider of its own: use ring for the process. An
    // `Err` means one is installed already, which is as good.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .redirect(redirects)
        .timeout(timeout)
        .connect_timeout(timeout)
        .user_agent(concat!("barca/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| {
            "cannot set up the HTTPS client (TLS or the system's root certificates)".to_string()
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn diagnostic_urls_omit_only_authority_credentials_even_when_invalid() {
        for (url, shown) in [
            ("http://alice:secret@host/proxy", "http://host/proxy"),
            ("https://token@host", "https://host"),
            (
                "http://a:s%40cret@[::1]:8274/barca/",
                "http://[::1]:8274/barca/",
            ),
            ("https://a:b@c@host/proxy", "https://host/proxy"),
            ("http://a:b@host:bad/proxy", "http://host:bad/proxy"),
            ("a:b@host", "host"),
            ("ftp://a:b@host", "ftp://host"),
            (
                "https://host/path@revision?q=a@b#c@d",
                "https://host/path@revision?q=a@b#c@d",
            ),
            ("https://host/proxy", "https://host/proxy"),
            ("", ""),
        ] {
            assert_eq!(super::diagnostic_url(url), shown);
        }
    }
}
