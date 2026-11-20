//! `barca pin` and `barca unpin`: hold an asset at an earlier materialization (`barca docs pins`).

use crate::error::CliError;
use crate::print::{print_pinned, print_pins, print_unpinned, print_versions};
use crate::request::{PinAction, read_supplied};
use std::path::PathBuf;

fn engine(e: barca_service::BarcaError) -> CliError {
    CliError::from_barca(e, &Default::default())
}

pub(crate) async fn pin_cmd(
    env: Option<&str>,
    action: &PinAction,
    files: Vec<PathBuf>,
    json: bool,
    python: &std::path::Path,
) -> Result<(), CliError> {
    let cfg = barca_service::config::resolve(env).map_err(engine)?;
    if let PinAction::List = action {
        let pins = barca_service::pins::list(&cfg.db_path)
            .await
            .map_err(engine)?;
        print_pins(&pins, json);
        return Ok(());
    }
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let dag = barca_service::load::build_dag(&file_args, python)
        .await
        .map_err(engine)?;
    match action {
        PinAction::List => unreachable!(),
        PinAction::Versions { target, limit } => {
            let versions = barca_service::pins::versions(&cfg.db_path, &dag, target, *limit)
                .await
                .map_err(engine)?;
            print_versions(&versions, json);
        }
        PinAction::Pin {
            target,
            choice,
            reason,
        } => {
            barca_service::queries::open_metadata(&cfg)
                .await
                .map_err(engine)?;
            let pinned = barca_service::pins::pin(
                &cfg.db_path,
                &dag,
                target,
                choice,
                reason.clone(),
                barca_service::pins::local_actor(),
            )
            .await
            .map_err(engine)?;
            print_pinned(&pinned, json);
        }
        PinAction::Upload {
            target,
            path,
            reason,
        } => {
            let supplied = read_supplied(path)?;
            barca_service::queries::open_metadata(&cfg)
                .await
                .map_err(engine)?;
            let pinned = barca_service::pins::pin_upload(
                &cfg,
                python,
                &dag,
                target,
                supplied,
                reason.clone(),
                barca_service::pins::local_actor(),
            )
            .await
            .map_err(engine)?;
            print_pinned(&pinned, json);
        }
    }
    Ok(())
}

pub(crate) async fn unpin_cmd(
    env: Option<&str>,
    target: &str,
    files: Vec<PathBuf>,
    json: bool,
    python: &std::path::Path,
) -> Result<(), CliError> {
    let cfg = barca_service::config::resolve(env).map_err(engine)?;
    let file_args: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    let dag = barca_service::load::build_dag(&file_args, python)
        .await
        .map_err(engine)?;
    let unpinned = barca_service::pins::unpin(&cfg.db_path, &dag, target)
        .await
        .map_err(engine)?;
    print_unpinned(&unpinned, json);
    Ok(())
}
