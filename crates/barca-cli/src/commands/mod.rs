mod execute;
mod plan;
pub(crate) use plan::*;
mod sql;
pub(crate) use sql::*;
mod list;
pub(crate) use list::*;
mod status;
pub(crate) use status::*;
mod history;
pub(crate) use history::*;
mod stats;
pub(crate) use stats::*;
mod serve;
pub(crate) use serve::*;
mod get;
pub(crate) use get::*;
mod run;
pub(crate) use run::*;

/// Emit ordered rendering writes. Flush stdout before stderr to retain result/error order.
fn emit(rendered: barca_core::report::RenderedOutput) {
    use barca_core::report::OutputChunk;
    use std::io::Write;
    for chunk in rendered.chunks {
        match chunk {
            OutputChunk::Stdout(text) => print!("{text}"),
            OutputChunk::Stderr(text) => {
                let _ = std::io::stdout().flush();
                eprint!("{text}");
            }
        }
    }
    let _ = std::io::stdout().flush();
}
fn project_root() -> Option<String> {
    std::env::current_dir()
        .ok()
        .map(|root| root.display().to_string())
}
