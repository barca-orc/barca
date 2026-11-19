//! The web UI only renders (#959; DIRECTION.md "The UI", `specs/behavior/health.md`): every
//! derived answer is computed in `barca-core`, and an agent on the CLI can get everything the
//! UI shows. The table of that rule is `crates/barca-cli/ui-layering.json`, held from two sides:
//!
//! - `scripts/check-ui-layering.py` (a prek hook: every pull request, every merge group,
//!   `scripts/check-fast.sh`) reads `ui/src`: every route the UI calls is in the table, every
//!   module of `ui/src/lib` has a kind and does only what its kind may, no word of barca's
//!   vocabulary (a health, a cause, a cache state, a run's or a step's status) is decided on
//!   outside a display module, and every count, order or produced word it can see is
//!   registered under `derives` with why it stays or the issue that moves it here.
//! - this test (`cargo test -p barca ui_layering`, the merge tier's Rust tests) reads the
//!   table against the CLI and the server: each route's `cli` is a command line the real CLI
//!   parses, each route is one the server serves with that method, and each exemption says
//!   why.
//!
//! Neither can check that a command prints the same JSON as its route: the table is a claim a
//! reviewer reads. Adding a CLI command for an exempted route, or moving a derivation into
//! `barca-core`, is the fix for a failure here; widening the table is not. How to extend it:
//! `site/src/content/docs/contributing/ui-layering.md`.

use super::*;
use std::collections::BTreeSet;
use std::path::Path;

fn table() -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("ui-layering.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// `/assets/{name}/pin` is `/assets/{}/pin`: the table names no parameter.
fn anonymous(path: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for c in path.chars() {
        match c {
            '{' => {
                inside = true;
                out.push_str("{}");
            }
            '}' => inside = false,
            _ if inside => {}
            _ => out.push(c),
        }
    }
    out
}

/// Every `(METHOD, path)` the router in `source` serves, read from its `.route(...)` calls.
fn served(source: &str) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let mut rest = source;
    while let Some(at) = rest.find(".route(") {
        rest = &rest[at + ".route(".len()..];
        // The call's text: up to the parenthesis that closes it.
        let mut depth = 1;
        let mut end = rest.len();
        for (i, c) in rest.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                end = i;
                break;
            }
        }
        let call = &rest[..end];
        let Some(open) = call.find('"') else { continue };
        let Some(len) = call[open + 1..].find('"') else {
            continue;
        };
        let path = anonymous(&call[open + 1..open + 1 + len]);
        let handlers = &call[open + 1 + len..];
        for (method, name) in [
            ("GET", "get("),
            ("POST", "post("),
            ("DELETE", "delete("),
            ("PUT", "put("),
        ] {
            if handlers.contains(name) {
                out.insert((method.to_string(), path.clone()));
            }
        }
    }
    out
}

#[test]
fn ui_layering_every_route_the_ui_calls_maps_to_the_cli_or_says_why_not() {
    let table = table();
    let routes = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../barca-server/src/routes.rs"),
    )
    .unwrap();
    let served = served(&routes);
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    for route in table["routes"].as_array().unwrap() {
        let method = route["method"].as_str().unwrap();
        let path = route["path"].as_str().unwrap();
        let name = format!("{method} {path}");
        if !seen.insert(name.clone()) {
            problems.push(format!("  {name} is listed twice"));
        }
        if !served.contains(&(method.to_string(), path.to_string())) {
            problems.push(format!(
                "  {name} is not a route of crates/barca-server/src/routes.rs"
            ));
        }
        match (route["cli"].as_str(), route.get("exempt")) {
            (Some(cli), None) => {
                if !cli.starts_with("barca ") {
                    problems.push(format!("  {name}: `{cli}` is not a barca command line"));
                } else if let Err(e) = Cli::try_parse_from(cli.split_whitespace()) {
                    problems.push(format!(
                        "  {name}: the CLI does not parse `{cli}`: {}",
                        e.to_string().lines().next().unwrap_or_default()
                    ));
                }
            }
            (None, Some(exempt)) => {
                if exempt["reason"]
                    .as_str()
                    .is_none_or(|r| r.trim().is_empty())
                {
                    problems.push(format!("  {name} is exempt without a reason"));
                }
                if exempt
                    .get("issue")
                    .is_some_and(|i| i.as_u64().is_none_or(|n| n == 0))
                {
                    problems.push(format!("  {name}: `issue` is a GitHub issue number"));
                }
            }
            _ => problems.push(format!("  {name} needs exactly one of `cli` and `exempt`")),
        }
    }
    assert!(
        problems.is_empty(),
        "UI layering (#959): every route the web UI calls maps to the CLI command that prints \
         the same answer, or is exempt with a reason (crates/barca-cli/ui-layering.json). Add \
         the command, or file an issue and exempt the route by its number.\n{}",
        problems.join("\n")
    );
}

#[test]
fn ui_layering_the_router_is_read_as_written() {
    let source = r#"
        .route("/runs", get(runs::list).post(runs::create))
        .route(
            "/auth/tokens/{id}",
            axum::routing::delete(crate::auth_http::revoke),
        )
        .route("/get/{*target}", post(runs::get_target))
    "#;
    let found = served(source);
    let found: Vec<(&str, &str)> = found
        .iter()
        .map(|(m, p)| (m.as_str(), p.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            ("DELETE", "/auth/tokens/{}"),
            ("GET", "/runs"),
            ("POST", "/get/{}"),
            ("POST", "/runs")
        ]
    );
}

/// A seeded violation: a route mapped to a command the CLI does not have is caught by the
/// same parse the table goes through.
#[test]
fn ui_layering_a_command_the_cli_does_not_have_is_caught() {
    assert!(Cli::try_parse_from("barca status --json".split_whitespace()).is_ok());
    assert!(Cli::try_parse_from("barca status --rollup --json".split_whitespace()).is_ok());
    assert!(Cli::try_parse_from("barca no-such-command".split_whitespace()).is_err());
    assert!(Cli::try_parse_from("barca status --no-such-flag".split_whitespace()).is_err());
}
