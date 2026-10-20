//! barca's outgoing HTTPS client.
//!
//! Every outgoing HTTPS request goes through [`client`], so the TLS stack (rustls with the
//! `ring` provider, never native-tls) and its root certificates are set up in one place.
//! Roots come from the operating system's store, through `rustls-platform-verifier`, so a
//! corporate proxy's CA that the machine trusts is trusted here too.

use std::time::Duration;

/// A client for HTTPS requests, each bounded by `timeout` from connecting to the last byte of
/// the reply. Building one reads the system's root certificates, so build it once and reuse it.
///
/// The error says why no client could be built, without naming any URL.
pub fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    // reqwest is built without a crypto provider of its own: use ring for the process. An
    // `Err` means one is installed already, which is as good.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(timeout)
        .user_agent(concat!("barca/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| {
            "cannot set up the HTTPS client (TLS or the system's root certificates)".to_string()
        })
}
