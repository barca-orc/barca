use super::follow::dropped;
use super::*;
use crate::error::ErrorKind;
use barca_core::status::CacheState;
use serde_json::json;

#[test]
fn a_newer_servers_unknown_cache_state_still_parses() {
    // Enum values are additive (contract): a state this CLI does not know is `unknown`,
    // with the server's reason and detail kept.
    let body = json!({
        "target": null,
        "nodes": [{
            "id": "p.py:a", "name": "a", "kind": "asset", "inputs": [],
            "partitioned": false,
            "cache": {"state": "future_state", "reason": "future_reason", "detail": "why"},
            "last_materialization": null, "shape": null
        }],
        "summary": {"cached": 0, "unsynced": 0, "stale": 0, "never_run": 0, "partial": 0,
                    "blocked": 0, "unknown": 1, "always_runs": 0}
    });
    let result: barca_core::status::StatusResult = serde_json::from_value(body).unwrap();
    let cache = &result.nodes[0].cache;
    assert_eq!(cache.state, CacheState::Unknown);
    assert_eq!(
        (cache.reason.as_str(), cache.detail.as_str()),
        ("future_reason", "why")
    );
}

#[test]
fn unexpected_answers_and_dropped_streams_keep_credentials_out_of_every_error_field() {
    let server = Server::new("http://alice:secret@host/proxy".into(), None, false).unwrap();
    for error in [
        server.unexpected("GET /health", "HTTP 404"),
        dropped(&server, "run", "the stream closed"),
    ] {
        let json = serde_json::to_string(&error).unwrap();
        assert!(!json.contains("secret"));
        assert!(!json.contains("alice"));
        assert!(json.contains("http://host/proxy"));
    }
    assert_eq!(
        server
            .http
            .request(reqwest::Method::GET, "/health")
            .unwrap()
            .build()
            .unwrap()
            .url()
            .as_str(),
        "http://host/proxy/health"
    );
    assert_eq!(
        barca_core::ledger::LedgerTransport::url(&server),
        "http://alice:secret@host/proxy"
    );
}

fn argv(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
}

#[test]
fn bare_server_before_a_positional_is_the_configured_server() {
    assert_eq!(
        bare_server_flags(argv("barca get --server total p.py")),
        argv("barca get --server= total p.py")
    );
    assert_eq!(
        bare_server_flags(argv("barca get total --server")),
        argv("barca get total --server=")
    );
    assert_eq!(
        bare_server_flags(argv("barca get total --server https://h/x --json")),
        argv("barca get total --server https://h/x --json")
    );
    assert_eq!(
        bare_server_flags(argv("barca get total --server --json")),
        argv("barca get total --server= --json")
    );
    assert_eq!(
        bare_server_flags(argv("barca get total --server ftp://x")),
        argv("barca get total --server ftp://x")
    );
    assert_eq!(
        bare_server_flags(argv("barca get -- --server")),
        argv("barca get -- --server")
    );
}

#[test]
fn local_commands_refuse_server_and_local() {
    for line in [
        "barca serve p.py --server",
        "barca plan --server=http://h",
        "barca plan p.py --local",
        "barca docs --server",
        "barca version --server",
    ] {
        let err = refuse_on_local_commands(&argv(line)).expect(line);
        assert_eq!(err.kind, ErrorKind::Usage, "{line}");
        assert!(err.error.contains("always runs on this machine"), "{line}");
    }
    for line in [
        "barca get x --server",
        "barca status --local",
        "barca sql q --local",
        "barca sql q --server",
        "barca p.py --server",
        "barca serve p.py",
    ] {
        assert!(refuse_on_local_commands(&argv(line)).is_none(), "{line}");
    }
}

#[test]
fn node_ids_are_filtered_by_file_and_encoded() {
    let files = vec!["a.py".to_string()];
    assert!(in_files("a.py:x", &files));
    assert!(in_files("a.py:x[k=1]", &files));
    assert!(!in_files("b.py:x", &files));
    assert!(in_files("b.py:x", &[]));
    assert_eq!(encode("sub/a.py:x y"), "sub%2Fa.py:x%20y");
}
