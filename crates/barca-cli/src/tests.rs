use super::*;
use clap::CommandFactory;

/// Subcommands that must carry runnable examples in their `--help`.
const DOCUMENTED: &[&str] = &[
    "get", "run", "plan", "history", "stats", "serve", "list", "status", "docs",
];

fn after_help(cmd: &clap::Command) -> String {
    cmd.get_after_help()
        .or_else(|| cmd.get_after_long_help())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// `barca ...` command lines found in `text`: indented example lines in help text, or
/// lines inside ```bash fences in a docs topic. Trailing `# comments` and anything after a
/// pipe (`barca ... | jq ...`) are stripped.
fn command_lines(text: &str, only_in_bash_fences: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_bash = false;
    for line in text.lines() {
        let t = line.trim();
        if only_in_bash_fences {
            if t.starts_with("```") {
                in_bash = t == "```bash";
                continue;
            }
            if !in_bash {
                continue;
            }
        }
        if t.starts_with("barca ") {
            let cmd = t.split(" #").next().unwrap_or(t);
            let cmd = cmd.split(" | ").next().unwrap_or(cmd).trim();
            out.push(cmd.to_string());
        }
    }
    out
}

fn assert_parses(cmd_line: &str, ctx: &str) {
    let argv = cmd_line.split_whitespace();
    match Cli::try_parse_from(argv) {
        Ok(_) => {}
        // `--help` / `--version` "fail" with a display error; that is a valid command.
        Err(e)
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) => {}
        Err(e) => panic!("{ctx}: `{cmd_line}` does not parse:\n{e}"),
    }
}

#[test]
fn every_documented_subcommand_has_examples_and_a_docs_pointer() {
    let root = Cli::command();
    for name in DOCUMENTED {
        let sub = root
            .find_subcommand(name)
            .unwrap_or_else(|| panic!("missing subcommand {name}"));
        let help = after_help(sub);
        assert!(
            help.contains("Examples:"),
            "`barca {name} --help` needs an `Examples:` section (after_help)"
        );
        assert!(
            !command_lines(&help, false).is_empty(),
            "`barca {name} --help` examples need at least one `barca ...` line"
        );
    }
}

#[test]
fn top_level_help_points_at_the_manual() {
    let help = after_help(&Cli::command());
    assert!(
        help.contains("barca docs"),
        "top-level --help must mention `barca docs`"
    );
    assert!(
        help.contains("barca docs agents"),
        "top-level --help must point agents at `barca docs agents`"
    );
}

#[test]
fn every_help_example_parses_against_the_real_cli() {
    let root = Cli::command();
    for name in DOCUMENTED {
        let help = after_help(root.find_subcommand(name).unwrap());
        for line in command_lines(&help, false) {
            assert_parses(&line, &format!("`barca {name} --help` example"));
        }
    }
    for line in command_lines(&after_help(&root), false) {
        assert_parses(&line, "top-level --help example");
    }
}

#[test]
fn every_docs_topic_command_parses_against_the_real_cli() {
    for t in docs::TOPICS {
        for line in command_lines(t.body, true) {
            assert_parses(&line, &format!("docs topic '{}'", t.name));
        }
    }
}

#[test]
fn docs_pointers_in_help_text_resolve_to_topics() {
    let root = Cli::command();
    let mut texts = vec![after_help(&root)];
    for name in DOCUMENTED {
        texts.push(after_help(root.find_subcommand(name).unwrap()));
    }
    for text in texts {
        for topic in docs::referenced_topics(&text) {
            assert!(
                docs::find(&topic).is_some(),
                "--help mentions unknown `barca docs {topic}`"
            );
        }
    }
}

#[test]
fn every_flag_and_argument_has_help_text() {
    fn check(cmd: &clap::Command, path: &str) {
        for arg in cmd.get_arguments() {
            if arg.is_hide_set() || matches!(arg.get_id().as_str(), "help" | "version") {
                continue;
            }
            assert!(
                arg.get_help().is_some() || arg.get_long_help().is_some(),
                "`{path}` argument '{}' has no help text — add a doc comment",
                arg.get_id()
            );
        }
        for sub in cmd.get_subcommands() {
            check(sub, &format!("{path} {}", sub.get_name()));
        }
    }
    check(&Cli::command(), "barca");
}

#[test]
fn every_subcommand_has_a_one_line_description() {
    for sub in Cli::command().get_subcommands() {
        assert!(
            sub.get_about().is_some(),
            "`barca {}` has no description",
            sub.get_name()
        );
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn files_before_target_prints_the_corrected_command() {
    let msg = wrong_order_error(
        "run",
        &strings(&["pipeline.py", "validate_foo"]),
        &strings(&["run", "pipeline.py", "validate_foo"]),
    )
    .expect("wrong order must be an error");
    assert_eq!(
        msg,
        "error: the target comes before the files\n\n  barca run validate_foo pipeline.py\n\n\
             Run `barca list pipeline.py` to see available assets and tasks."
    );
}

#[test]
fn corrected_command_keeps_flags_and_handles_the_shorthand() {
    let msg = wrong_order_error(
        "get",
        &strings(&["a.py", "total", "b.py"]),
        &strings(&["a.py", "total", "b.py", "--env", "dev", "-o", "value"]),
    )
    .unwrap();
    assert!(
        msg.contains("\n  barca get total a.py b.py --env dev -o value\n"),
        "{msg}"
    );
    assert!(msg.ends_with("Run `barca list a.py b.py` to see available assets and tasks."));
}

#[test]
fn several_non_py_positionals_after_a_file_do_not_guess() {
    let msg = wrong_order_error(
        "run",
        &strings(&["pipeline.py", "report", "mid"]),
        &strings(&["run", "pipeline.py", "report", "mid"]),
    )
    .unwrap();
    assert!(msg.starts_with("error: the target comes before the files"));
    assert!(!msg.contains("barca run report pipeline.py"), "{msg}");
    assert!(msg.contains("Usage: barca run <TARGET> <FILES>..."));
}

#[test]
fn correct_order_and_files_only_are_not_wrong_order() {
    for args in [
        &["report", "pipeline.py"][..],
        &["pipeline.py"][..],
        &["a.py", "b.py"][..],
        &["report", "pipeline.py", "mid"][..], // `--refresh a b`: handled by check_py_files
    ] {
        let a = strings(args);
        assert!(wrong_order_error("run", &a, &a).is_none(), "{args:?}");
    }
}

/// barca never offers fuzzy suggestions (`barca docs agents`): a guess can read as
/// confirmation. clap is built without its `suggestions` feature.
#[test]
fn argument_errors_carry_no_similar_argument_tip() {
    for argv in [
        "barca get total p.py --jsn",
        "barca run t p.py --refesh a",
        "barca lst p.py",
    ] {
        let Err(e) = Cli::try_parse_from(argv.split_whitespace()) else {
            panic!("`{argv}` must not parse");
        };
        let text = CliError::from_clap(&e).render(true).to_lowercase();
        assert!(!text.contains("similar"), "{argv}: {text}");
        assert!(!text.contains("did you mean"), "{argv}: {text}");
    }
}

#[test]
fn unknown_target_remediation_is_one_wording_on_every_command() {
    let files = vec!["pipeline.py".to_string()];
    for command in ["get", "run", "status", "stats"] {
        let e = CliError::from_barca(
            barca_core::BarcaError::AssetNotFound("nope".into(), "pipeline.py:a".into()),
            &Context {
                command,
                files: files.clone(),
            },
        );
        assert_eq!(
            e.remediation.as_deref(),
            Some("Run `barca list pipeline.py` to see available assets and tasks."),
            "{command}"
        );
    }
}

#[test]
fn list_hint_names_the_files_or_a_placeholder() {
    assert_eq!(
        list_hint(&[PathBuf::from("a.py"), PathBuf::from("my dir/b.py")]),
        "Run `barca list a.py 'my dir/b.py'` to see available assets and tasks."
    );
    assert_eq!(
        list_hint(&[]),
        "Run `barca list` to see available assets and tasks."
    );
}

#[test]
fn top_level_description_names_barca_list() {
    let cmd = Cli::command();
    for text in [cmd.get_about(), cmd.get_long_about()] {
        let text = text.map(|s| s.to_string()).unwrap_or_default();
        assert!(text.contains("barca list"), "{text}");
    }
}

#[test]
fn fields_flag_exists_on_every_command_with_json_output() {
    let root = Cli::command();
    for name in ["get", "run", "list", "history", "stats", "docs"] {
        let sub = root.find_subcommand(name).unwrap();
        assert!(
            sub.get_arguments().any(|a| a.get_id() == "fields"),
            "`barca {name}` emits JSON, so it needs --fields"
        );
    }
}

#[test]
fn list_shaped_commands_take_limit_and_all() {
    let root = Cli::command();
    for name in ["list", "history"] {
        let sub = root.find_subcommand(name).unwrap();
        for flag in ["limit", "all"] {
            assert!(
                sub.get_arguments().any(|a| a.get_id() == flag),
                "`barca {name}` is list-shaped, so it needs --{flag}"
            );
        }
    }
}

#[test]
fn a_target_argument_is_a_comma_separated_list() {
    assert_eq!(parse_targets("total").unwrap(), vec!["total"]);
    assert_eq!(parse_targets("a,b").unwrap(), vec!["a", "b"]);
    assert_eq!(parse_targets("b,a,b").unwrap(), vec!["b", "a"]);
    assert_eq!(
        parse_targets("pipeline.py:a,b").unwrap(),
        vec!["pipeline.py:a", "b"]
    );
    for bad in ["a,,b", "a,", ",a"] {
        let err = parse_targets(bad).unwrap_err();
        assert!(err.contains("empty target name"), "{bad}: {err}");
    }
}

#[test]
fn several_targets_split_from_the_files() {
    let (target, files) = split_target_files(vec!["a,b".into(), "pipeline.py".into()]);
    assert_eq!(target.as_deref(), Some("a,b"));
    assert_eq!(files, vec![PathBuf::from("pipeline.py")]);
}

#[test]
fn json_flags_exist_on_inspection_commands() {
    let root = Cli::command();
    for name in ["list", "history", "stats", "status", "docs"] {
        let sub = root.find_subcommand(name).unwrap();
        assert!(
            sub.get_arguments().any(|a| a.get_id() == "json"),
            "`barca {name}` needs a --json flag for machine-readable output"
        );
    }
}

/// One override family everywhere: every command that follows the TTY rule takes both
/// `--json` and `--pretty` (see output.rs), and the manual explains the rule.
#[test]
fn tty_aware_commands_take_json_and_pretty_and_the_rule_is_documented() {
    let root = Cli::command();
    for name in ["get", "run", "list", "history", "stats"] {
        let sub = root.find_subcommand(name).unwrap();
        for flag in ["json", "pretty"] {
            assert!(
                sub.get_arguments().any(|a| a.get_id() == flag),
                "`barca {name}` needs --{flag}"
            );
        }
    }
    for name in ["get", "run"] {
        let help = after_help(root.find_subcommand(name).unwrap());
        assert!(
            help.contains("--pretty"),
            "`barca {name} --help` needs a --pretty example"
        );
    }
    let agents = docs::find("agents").expect("agents topic").body;
    for needle in ["BARCA_OUTPUT", "--pretty", "--json", "terminal"] {
        assert!(
            agents.contains(needle),
            "`barca docs agents` must mention {needle}"
        );
    }
    assert!(after_help(&root).contains("BARCA_OUTPUT"));
}
