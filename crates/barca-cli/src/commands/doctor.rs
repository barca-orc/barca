//! Diagnostic report rendering; the core owns prerequisite checks.
use crate::error::{CliError, ErrorKind};
use std::path::PathBuf;

/// The checks of this machine (`barca_service::diagnostics`); `dispatch` adds a server's.
pub(crate) async fn doctor_report(
    files: &[PathBuf],
    env: Option<&str>,
) -> barca_service::diagnostics::DoctorReport {
    let python = barca_service::commands::find_python();
    barca_service::diagnostics::doctor(files, env, &python).await
}

/// Print the report; failed checks end the command with an infra error.
pub(crate) fn print_doctor(
    report: &barca_service::diagnostics::DoctorReport,
    json: bool,
) -> Result<(), CliError> {
    if json {
        barca_service::outln!("{}", serde_json::to_string_pretty(report).unwrap());
    } else {
        for check in &report.checks {
            let detail = check
                .detail
                .lines()
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("; ");
            barca_service::outln!(
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
