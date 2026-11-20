//! A minimal DogStatsD client: gauges and events as UDP (or Unix datagram) packets to the
//! Datadog Agent, for `barca serve`'s lifecycle.
//!
//! The address is Datadog's own settings, in this order:
//!
//! - `DD_DOGSTATSD_URL`: `udp://host:port` or `unix:///path/to/dsd.socket`;
//! - `DD_DOGSTATSD_SOCKET`: a Unix datagram socket path;
//! - `DD_AGENT_HOST` (default `localhost`) and `DD_DOGSTATSD_PORT` (default `8125`).
//!
//! Datagrams are fire-and-forget: an Agent that is not listening loses the metrics without
//! an error, as with every DogStatsD client.

use super::datadog::env_var;

/// Where DogStatsD packets go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Udp { host: String, port: u16 },
    Unix { path: String },
}

impl Target {
    pub(crate) fn from_env() -> Result<Self, String> {
        target_from(
            env_var("DD_DOGSTATSD_URL").as_deref(),
            env_var("DD_DOGSTATSD_SOCKET").as_deref(),
            env_var("DD_AGENT_HOST").as_deref(),
            env_var("DD_DOGSTATSD_PORT").as_deref(),
        )
    }
}

pub(crate) fn target_from(
    url: Option<&str>,
    socket: Option<&str>,
    host: Option<&str>,
    port: Option<&str>,
) -> Result<Target, String> {
    let parse_port = |raw: &str| {
        raw.parse::<u16>()
            .map_err(|_| "DogStatsD port is not a port number".to_string())
    };
    if let Some(url) = url {
        if let Some(path) = url.strip_prefix("unix://") {
            if path.is_empty() {
                return Err("DD_DOGSTATSD_URL=unix:// names no socket path".to_string());
            }
            return Ok(Target::Unix {
                path: path.to_string(),
            });
        }
        let Some(authority) = url.strip_prefix("udp://") else {
            return Err(
                "DD_DOGSTATSD_URL is not supported: use udp://host:port or unix:///path"
                    .to_string(),
            );
        };
        let authority = authority.trim_end_matches('/');
        let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
            let Some((host, after)) = rest.split_once(']') else {
                return Err("DD_DOGSTATSD_URL has an unclosed '['".to_string());
            };
            match after.strip_prefix(':') {
                Some(p) => (host, parse_port(p)?),
                None if after.is_empty() => (host, 8125),
                None => return Err("DD_DOGSTATSD_URL has unexpected text after ']'".to_string()),
            }
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h, parse_port(p)?),
                None => (authority, 8125),
            }
        };
        if host.is_empty() || host.contains('@') {
            return Err("DD_DOGSTATSD_URL names no host".to_string());
        }
        return Ok(Target::Udp {
            host: host.to_string(),
            port,
        });
    }
    if let Some(path) = socket {
        return Ok(Target::Unix {
            path: path.to_string(),
        });
    }
    Ok(Target::Udp {
        host: host.unwrap_or("localhost").to_string(),
        port: port.map(parse_port).transpose()?.unwrap_or(8125),
    })
}

/// A tag key or value with the characters DogStatsD uses as separators replaced.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            ',' | '|' | '#' | '\n' | '\r' => '_',
            c => c,
        })
        .collect()
}

fn tag_list(tags: &[(String, String)]) -> String {
    tags.iter()
        .map(|(k, v)| format!("{}:{}", clean(k), clean(v)))
        .collect::<Vec<_>>()
        .join(",")
}

/// `name:value|g|#k:v,...`
pub(crate) fn gauge(name: &str, value: f64, tags: &[(String, String)]) -> String {
    let mut packet = format!("{name}:{value}|g");
    if !tags.is_empty() {
        packet.push_str("|#");
        packet.push_str(&tag_list(tags));
    }
    packet
}

/// `_e{title length,text length}:title|text|t:info|#k:v,...`. Lengths are in bytes.
pub(crate) fn event(title: &str, text: &str, tags: &[(String, String)]) -> String {
    let title = title.replace('\n', " ");
    let text = text.replace('\n', "\\n");
    let mut packet = format!(
        "_e{{{},{}}}:{title}|{text}|t:info|s:barca",
        title.len(),
        text.len()
    );
    if !tags.is_empty() {
        packet.push_str("|#");
        packet.push_str(&tag_list(tags));
    }
    packet
}

/// Send each packet as one datagram.
pub(crate) async fn send(target: &Target, packets: &[String]) -> Result<(), String> {
    match target {
        Target::Udp { host, port } => {
            let addr = tokio::net::lookup_host((host.as_str(), *port))
                .await
                .map_err(|e| format!("cannot resolve the DogStatsD host {host}: {e}"))?
                .next()
                .ok_or_else(|| format!("the DogStatsD host {host} has no address"))?;
            let local: std::net::SocketAddr = if addr.is_ipv6() {
                (std::net::Ipv6Addr::UNSPECIFIED, 0).into()
            } else {
                (std::net::Ipv4Addr::UNSPECIFIED, 0).into()
            };
            let socket = tokio::net::UdpSocket::bind(local)
                .await
                .map_err(|e| e.to_string())?;
            for packet in packets {
                socket
                    .send_to(packet.as_bytes(), addr)
                    .await
                    .map_err(|e| format!("cannot send to DogStatsD at {addr}: {e}"))?;
            }
            Ok(())
        }
        Target::Unix { path } => {
            let socket = tokio::net::UnixDatagram::unbound().map_err(|e| e.to_string())?;
            for packet in packets {
                socket
                    .send_to(packet.as_bytes(), path)
                    .await
                    .map_err(|e| format!("cannot send to DogStatsD at {path}: {e}"))?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_comes_from_the_url_the_socket_or_host_and_port() {
        let udp = |host: &str, port| Target::Udp {
            host: host.to_string(),
            port,
        };
        assert_eq!(
            target_from(None, None, None, None),
            Ok(udp("localhost", 8125))
        );
        assert_eq!(
            target_from(None, None, Some("agent"), Some("9125")),
            Ok(udp("agent", 9125))
        );
        assert_eq!(
            target_from(Some("udp://dsd:8126"), Some("/ignored"), Some("x"), None),
            Ok(udp("dsd", 8126))
        );
        assert_eq!(
            target_from(Some("udp://[::1]:8125"), None, None, None),
            Ok(udp("::1", 8125))
        );
        assert_eq!(
            target_from(Some("udp://dsd"), None, None, None),
            Ok(udp("dsd", 8125))
        );
        assert_eq!(
            target_from(Some("unix:///var/run/datadog/dsd.socket"), None, None, None),
            Ok(Target::Unix {
                path: "/var/run/datadog/dsd.socket".to_string()
            })
        );
        assert_eq!(
            target_from(None, Some("/run/dsd.socket"), Some("agent"), None),
            Ok(Target::Unix {
                path: "/run/dsd.socket".to_string()
            })
        );
        assert!(target_from(Some("http://dsd:8125"), None, None, None).is_err());
        assert!(target_from(Some("udp://dsd:many"), None, None, None).is_err());
        assert!(target_from(None, None, None, Some("many")).is_err());
        let secret = target_from(Some("udp://user:secret@dsd:8125"), None, None, None);
        assert!(!secret.unwrap_err().contains("secret"));
    }

    #[test]
    fn gauges_and_events_use_the_dogstatsd_datagram_format() {
        let tags = vec![
            ("env".to_string(), "prod".to_string()),
            ("team".to_string(), "a,b|c".to_string()),
        ];
        assert_eq!(
            gauge("barca.serve.up", 1.0, &tags),
            "barca.serve.up:1|g|#env:prod,team:a_b_c"
        );
        assert_eq!(gauge("x", 0.0, &[]), "x:0|g");
        assert_eq!(
            event("barca serve started", "2 files\nok", &[]),
            "_e{19,11}:barca serve started|2 files\\nok|t:info|s:barca"
        );
    }

    #[tokio::test]
    async fn packets_arrive_as_one_datagram_each() {
        let listener = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let target = Target::Udp {
            host: "127.0.0.1".to_string(),
            port,
        };
        send(&target, &["a:1|g".to_string(), "b:2|g".to_string()])
            .await
            .unwrap();
        let mut buf = [0u8; 512];
        let n = listener.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"a:1|g");
        let n = listener.recv(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"b:2|g");
    }
}
