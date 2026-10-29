//! Help is an offline control path, even inside a broken project.

use std::process::Command;

#[test]
fn help_and_examples_never_load_a_project_or_require_python() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("barca.toml"), "not valid toml!").unwrap();
    std::fs::write(
        project.path().join("pipeline.py"),
        "raise RuntimeError('help must never import this')\n",
    )
    .unwrap();

    // Cover missing required arguments, nested state-changing commands, the server,
    // and the file shorthand handled before dispatch in main.
    for path in [
        vec![],
        vec!["get"],
        vec!["run"],
        vec!["serve"],
        vec!["state", "restore"],
        vec!["state", "reset"],
        vec!["pipeline.py"],
    ] {
        for flag in ["-h", "--help", "--examples"] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_barca"));
            command
                .args(&path)
                .arg(flag)
                .current_dir(project.path())
                .env("PATH", "")
                .env("BARCA_OUTPUT", "invalid")
                .env("BARCA_LOG", "invalid")
                .env("BARCA_SERVER", "http://127.0.0.1:1");
            let output = barca_core::helper_proc::output_std(&mut command).unwrap();
            assert!(output.status.success(), "{path:?} {flag}: {output:?}");
            assert!(output.stderr.is_empty(), "{path:?} {flag}: {output:?}");
            assert!(String::from_utf8(output.stdout).unwrap().contains("Usage:"));
        }
    }
    assert!(!project.path().join(".barca").exists());
}
