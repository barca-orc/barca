use super::*;
use clap::Parser;

fn request(line: &str, side: Side) -> Result<Request, CliError> {
    let argv: Vec<String> = line.split_whitespace().map(String::from).collect();
    let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{line}: {e}"));
    Request::of(cli, side)
}

fn usage(line: &str, side: Side) -> String {
    let Err(err) = request(line, side) else {
        panic!("{line} ({side:?}) is not a usage error");
    };
    assert_eq!(err.kind, ErrorKind::Usage, "{line}");
    err.render(false)
}

/// The one difference between the sides: the files. Here they are `.py` files and `get`,
/// `run` and `status` need one; a server takes a directory, or none.
#[test]
fn files_are_checked_for_the_side_they_go_to() {
    for command in ["get", "status"] {
        let none = usage(&format!("barca {command} total"), Side::Here);
        assert!(none.contains("no .py files provided"), "{none}");
        assert!(request(&format!("barca {command} total"), Side::Server).is_ok());
        assert!(request(&format!("barca {command} total sub/"), Side::Server).is_ok());
        let dir = usage(&format!("barca {command} total sub/"), Side::Here);
        assert!(dir.contains("'sub/' is not a .py file"), "{dir}");
        // Neither side takes a file that is not Python.
        for side in [Side::Here, Side::Server] {
            let bad = usage(&format!("barca {command} total p.py notes.txt"), side);
            assert!(bad.contains("'notes.txt' is not a .py file"), "{bad}");
        }
    }
    let none = usage("barca get", Side::Here);
    assert!(none.contains("error: no files provided"), "{none}");
    let none = usage("barca run deploy", Side::Here);
    assert!(none.contains("no .py files provided"), "{none}");
    assert!(request("barca run deploy", Side::Server).is_ok());
    // The commands that leave their files to `barca_service` here still check them for a server.
    for line in [
        "barca list notes.txt",
        "barca stats total notes.txt",
        "barca sql select notes.txt",
        "barca pin total notes.txt --previous",
        "barca unpin total notes.txt",
    ] {
        assert!(request(line, Side::Here).is_ok(), "{line}");
        let bad = usage(line, Side::Server);
        assert!(bad.contains("'notes.txt' is not a .py file"), "{bad}");
    }
}

/// What a server refuses, and this machine takes.
#[test]
fn two_flags_are_refused_for_a_server() {
    let url = usage("barca get total p.py -o url", Side::Server);
    assert!(
        url.contains("-o url cannot be used on a server yet"),
        "{url}"
    );
    let Ok(Request::Get { mode, .. }) = request("barca get total p.py -o url", Side::Here) else {
        panic!("get -o url is a request here");
    };
    assert_eq!(
        mode.url_expires,
        Some(crate::input::DEFAULT_URL_EXPIRES_SECS)
    );

    let reconcile = usage("barca history --reconcile r1", Side::Server);
    assert!(
        reconcile.contains("--reconcile records a run on this machine's history"),
        "{reconcile}"
    );
    let Ok(Request::History(history)) = request("barca history --reconcile r1", Side::Here) else {
        panic!("history --reconcile is a request here");
    };
    assert!(matches!(
        history.view,
        HistoryView::Runs { reconcile: Some(run), .. } if run == "r1"
    ));
}

/// Everything else is one check, with one text, for both sides.
#[test]
fn the_other_usage_errors_are_the_same_on_both_sides() {
    for line in [
        "barca run p.py",
        "barca get a,,b p.py",
        "barca get total p.py --partition",
        "barca get total p.py --expires 1h",
        "barca get total p.py -o json --json",
        "barca get total p.py --fields id --pretty",
        "barca run deploy p.py --fields id -o value",
        "barca status p.py --keys",
        "barca status a,b p.py --grid",
        "barca status p.py --state nonsense",
        "barca status total p.py --keys --sort nonsense",
        "barca list p.py --fields id --pretty",
        "barca stats total p.py --fields status --pretty",
        "barca history --since nonsense",
        "barca history --fields status --pretty",
        "barca pin --previous",
        "barca pin total",
    ] {
        let argv: Vec<String> = line.split_whitespace().map(String::from).collect();
        // A line clap itself refuses is not this module's to check.
        if Cli::try_parse_from(&argv).is_err() {
            continue;
        }
        assert_eq!(usage(line, Side::Here), usage(line, Side::Server), "{line}");
    }
}

#[test]
fn status_picks_one_view() {
    let view = |line: &str| match request(line, Side::Here) {
        Ok(Request::Status(status)) => status.view,
        _ => panic!("{line} is a status request"),
    };
    assert!(matches!(
        view("barca status total p.py --keys --limit 5"),
        StatusView::Keys { target, view: KeysView::Keys(_), limit: Some(5) } if target == "total"
    ));
    assert!(matches!(
        view("barca status total p.py --key day=1 --sample 3"),
        StatusView::Keys {
            view: KeysView::Key { sample: 3, .. },
            ..
        }
    ));
    assert!(matches!(
        view("barca status p.py --rollup --now"),
        StatusView::Rollup { now: true }
    ));
    assert!(matches!(
        view("barca status p.py --problems --all --sample 2"),
        StatusView::Nodes { limit: None, sample: 2, filter, .. } if filter.problems
    ));
}
