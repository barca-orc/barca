//! Locations as barca shows and reads them: a server URL or store URI with its credentials left
//! out, and the local file a recorded artifact path names.

use std::path::Path;

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

/// Display a location without URI passwords or signed query parameters.
pub fn diagnostic_uri(uri: &str) -> String {
    if local_path(uri).is_some() {
        return uri.to_string();
    }
    let plain = uri.split(['?', '#']).next().unwrap_or(uri);
    if let Some((scheme, rest)) = plain.split_once("://")
        && let Some((credentials, host)) = rest.split_once('@')
        && credentials.contains(':')
    {
        return format!("{scheme}://<redacted>@{host}");
    }
    plain.to_string()
}

/// The file a recorded artifact path names on this machine: a plain path or a
/// `file://` URI. None for a remote URI. The same rule as
/// `barca._storage.local_path_of`, which is what a worker applies to the path.
pub(crate) fn local_path(recorded: &str) -> Option<&Path> {
    if let Some(file) = recorded.strip_prefix("file://") {
        return Some(Path::new(file));
    }
    (!recorded.contains("://")).then(|| Path::new(recorded))
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
