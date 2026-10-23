//! Diagnostic report rendering; the core owns prerequisite checks.
use crate::error::{CliError, ErrorKind};
use std::path::{Path, PathBuf};

#[allow(clippy::result_large_err)] // one diagnostic envelope on the command failure path
pub(crate) async fn doctor_cmd(
    files: Vec<PathBuf>,
    env: Option<&str>,
    python: &Path,
    json: bool,
    server: Option<crate::client::Server>,
) -> Result<(), CliError> {
    let mut report = barca_core::diagnostics::doctor(&files, env, python).await;
    if let Some(server) = &server {
        crate::client::doctor_checks(server, &mut report, env).await;
        report.ok = report.checks.iter().all(|check| check.ok);
    }
    if json {
        barca_core::outln!("{}", serde_json::to_string_pretty(&report).unwrap());
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
