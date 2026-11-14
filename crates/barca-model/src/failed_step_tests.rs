use super::*;

fn step(message: &str) -> FailedStep {
    FailedStep {
        node: "p.py:oops".into(),
        message: message.into(),
        phase: None,
        artifact_dir: None,
        run: None,
    }
}

#[test]
fn splits_exception_line_from_traceback_frames() {
    let s = step(
        "ZeroDivisionError: division by zero\n  File \"p.py\", line 5, in oops\n    return 1 / 0\n",
    );
    assert_eq!(s.summary(), "ZeroDivisionError: division by zero");
    assert_eq!(
        s.traceback(),
        Some("  File \"p.py\", line 5, in oops\n    return 1 / 0")
    );
}

#[test]
fn multi_line_message_without_traceback() {
    let s = step("worker disconnected");
    assert_eq!(s.summary(), "worker disconnected");
    assert_eq!(s.traceback(), None);
    let s = step("ValueError: bad\nsecond line");
    assert_eq!(s.summary(), "ValueError: bad\nsecond line");
}

#[test]
fn safe_node_id_matches_the_python_artifact_layout() {
    assert_eq!(safe_node_id("pipeline.py:fetch"), "pipeline.py--fetch");
    assert_eq!(
        safe_node_id("src/p.py:f[region=eu,day=1]"),
        "src__p.py--f_region_eu_day_1"
    );
}
