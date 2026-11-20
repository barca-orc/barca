//! The `.barca/` directory of a project: where an environment's metadata database and
//! artifacts live, and the entries barca itself creates there
//! (`specs/interfaces/dot-barca-layout.md`). Path derivation has no side effects;
//! [`ensure_env_dirs`] creates the tree.

use crate::BarcaError;
use std::fs;
use std::path::{Path, PathBuf};

/// Local filesystem locations for one environment.
#[derive(Debug, Clone)]
pub struct LocalPaths {
    pub db_path: String,
    pub artifact_dir: String,
}

/// Path derivation for an environment, with no filesystem side effects.
/// The default env keeps the legacy layout so existing projects need no
/// migration; named envs live under `.barca/envs/<name>/`.
pub fn env_local_paths(env: &str) -> LocalPaths {
    let base = if env == crate::config::DEFAULT_ENV {
        PathBuf::from(".barca")
    } else {
        PathBuf::from(".barca").join("envs").join(env)
    };
    LocalPaths {
        db_path: base.join("metadata.db").to_string_lossy().to_string(),
        artifact_dir: base.join("artifacts").to_string_lossy().to_string(),
    }
}

/// The file name of the Unix socket `barca serve` listens on, beside its environment's metadata
/// database (`specs/interfaces/dot-barca-layout.md`).
pub const SERVE_SOCKET: &str = "serve.sock";

/// Where the server that owns the database at `db_path` listens: `serve.sock` beside it.
pub fn serve_socket_path(db_path: &str) -> PathBuf {
    Path::new(db_path)
        .parent()
        .map_or_else(|| PathBuf::from(SERVE_SOCKET), |d| d.join(SERVE_SOCKET))
}

/// Create the `.barca` tree for an environment and return its paths.
pub fn ensure_env_dirs(env: &str) -> Result<LocalPaths, BarcaError> {
    let paths = env_local_paths(env);
    let db_dir = Path::new(&paths.db_path)
        .parent()
        .expect("db path has a parent")
        .to_path_buf();
    fs::create_dir_all(&db_dir)
        .map_err(|e| BarcaError::Db(format!("failed to create {}: {e}", db_dir.display())))?;
    fs::create_dir_all(&paths.artifact_dir)
        .map_err(|e| BarcaError::Db(format!("failed to create artifacts dir: {e}")))?;
    let gitignore = PathBuf::from(".barca").join(".gitignore");
    if !gitignore.exists() {
        let _ = fs::write(&gitignore, "*\n");
    }
    Ok(paths)
}

/// The file under `.barca/` in which the cache size limit keeps its last full walk of the
/// directory (`gc`). It is named here, below both of its users: `gc` reads and writes it, and
/// whatever replaces the metadata database (`barca state reset`, `barca state restore`)
/// removes it with [`forget_gc_survey`].
pub const GC_STATE_FILE: &str = "gc-state.json";

/// Forget the last full walk of `barca_dir` (a project's `.barca/`), so the next check walks
/// it again: the recorded sizes the fast estimate adds up no longer describe the files on
/// disk. A missing file is already forgotten.
pub fn forget_gc_survey(barca_dir: &Path) {
    let _ = fs::remove_file(barca_dir.join(GC_STATE_FILE));
}
