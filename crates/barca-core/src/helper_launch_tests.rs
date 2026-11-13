//! A source scan: every Python helper barca starts goes through `helper_proc`'s shared launch
//! path and forwards the resolved storage options. It reads production sources of
//! `barca-engine` (the worker) and `barca-core` (the inspectors and helpers), so it lives here,
//! above both.

/// Every production helper must use the shared launch path and pass the resolved options.
/// This guards the wiring as well as the subprocess round trip in `helper_proc`'s tests: testing the builder
/// alone would still pass if a new worker or inspector bypassed it.
#[test]
fn every_helper_launch_forwards_resolved_storage_options() {
    for (source, module) in [
        (
            include_str!("../../barca-engine/src/io_loop/lifecycle.rs"),
            "barca._worker",
        ),
        (include_str!("backup.rs"), "barca._state"),
        (include_str!("diagnostics.rs"), "barca._doctor"),
        (include_str!("store_inspect.rs"), "barca._inspect"),
        (include_str!("values.rs"), "barca._value"),
        (include_str!("sql.rs"), "barca._sql"),
        (include_str!("transfer.rs"), "barca._transfer"),
    ] {
        let production = source.split("mod tests {").next().unwrap();
        let code = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let literal = format!("\"{module}\"");
        let mut count = 0;
        for (offset, _) in code.match_indices(&literal) {
            count += 1;
            let before = &code[..offset];
            let call = before
                .rsplit_once("crate::helper_proc::python_module")
                .unwrap_or_else(|| panic!("{module} bypasses shared helper configuration"));
            assert!(
                !call.1.contains(';'),
                "{module} bypasses the shared builder"
            );
            let after = &code[offset + literal.len()..];
            let arguments = after.split(";").next().unwrap();
            assert!(
                arguments.contains("storage_options_json.as_deref()"),
                "{module} does not forward resolved storage options"
            );
        }
        assert_eq!(count, 1, "expected one production launcher for {module}");
    }
}
