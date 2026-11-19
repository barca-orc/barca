//! Diagnostic report rendering; the core owns prerequisite checks.
use crate::error::{CliError, ErrorKind};
use std::path::PathBuf;

/// The checks of this machine (`barca_core::diagnostics`); `dispatch` adds a server's.
pub(crate) async fn doctor_report(
    files: &[PathBuf],
    env: Option<&str>,
) -> barca_core::diagnostics::DoctorReport {
    let python = barca_core::commands::find_python();
    barca_core::diagnostics::doctor(files, env, &python).await
}

/// Print the report; failed checks end the command with an infra error.
pub(crate) fn print_doctor(
    report: &barca_core::diagnostics::DoctorReport,
    json: bool,
) -> Result<(), CliError> {
    if json {
        barca_core::outln!("{}", serde_json::to_string_pretty(report).unwrap());
    } else {
        for check in &report.checks {
            let detail = check
                .detail
                .lines()
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("; ");
            barca_core::outln!(
                "[{}] {}: {}",
                if check.ok { "ok" } else { "error" },
                check.name,
                detail
            );
        }
    }
    if report.ok {
        Ok(())
    } else {
        Err(CliError::from_prose(
            ErrorKind::Infra,
            "doctor found failed checks\nFix the failed checks in the diagnostic report and rerun barca doctor.",
        ))
    }
}
