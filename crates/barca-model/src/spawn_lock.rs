//! Starting a child process one at a time, across the whole program (#293): the lock that
//! `helper_proc::spawn` and [`spawn_std`] hold while a child is started.
//!
//! A process primitive below every layer, so that loading a pipeline (the partition-key
//! resolver, `batches(...)`) starts its subprocesses under the same lock as the engine's
//! workers and helpers. See `crate::helper_proc` for why the lock exists.

/// Held while a child process is being started.
pub static SPAWNING: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `helper_proc::spawn` for a `std::process::Command`.
pub fn spawn_std(cmd: &mut std::process::Command) -> std::io::Result<std::process::Child> {
    let _one_at_a_time = SPAWNING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cmd.spawn()
}

/// Run `cmd` to completion and collect its output, started through [`spawn_std`]: what
/// `Command::output` does, one at a time.
pub fn output_std(cmd: &mut std::process::Command) -> std::io::Result<std::process::Output> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    spawn_std(cmd)?.wait_with_output()
}
