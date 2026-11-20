//! The project socket: a Unix socket `barca serve` listens on beside its environment's
//! metadata database (`.barca/serve.sock`, or `.barca/envs/<env>/serve.sock`), serving the same
//! API as its TCP address. A `barca` command run in the project finds the server by it and
//! sends its work there instead of opening the database beside the server
//! (`barca docs server-mode`, `specs/behavior/server-authentication.md`).
//!
//! Access is by file permission: the socket is owner-only (`0600`), and a request that comes
//! over it needs no token even when tokens are enabled ([`ViaProjectSocket`]), because whoever
//! can open it can already write the database it stands beside.
//!
//! Only the server that holds the environment's `serve.lock` opens the socket, so a socket
//! file found at startup is a dead server's and is replaced. It is removed when the server
//! exits; one a killed server left refuses connections, which a command reads as "no server".

use std::path::{Path, PathBuf};

/// Marks a request that came over the project socket (a request extension set on that
/// listener's router only).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ViaProjectSocket;

/// The most bytes a Unix socket path may have, the terminating NUL not counted: `sun_path` is
/// 108 bytes on Linux and 104 on the BSDs and macOS.
const MAX_SOCKET_PATH: usize = if cfg!(target_os = "linux") { 107 } else { 103 };

/// The bound socket, removed when dropped.
pub(crate) struct ProjectSocket {
    path: PathBuf,
    /// The socket file's device and inode: only this server's own file is removed on exit.
    identity: (u64, u64),
    listener: Option<tokio::net::UnixListener>,
}

impl ProjectSocket {
    /// Bind the socket beside `db_path` and make it owner-only before any other process can
    /// reach it: it is bound inside a private (`0700`) directory, set to `0600`, then renamed
    /// into place, which also replaces a dead server's socket in one step.
    ///
    /// The error is one line saying why this server has no project socket.
    #[cfg(unix)]
    pub(crate) fn open(db_path: &str) -> Result<Self, String> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let path = barca_service::db::serve_socket_path(db_path);
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let staging = dir.join(format!(".serve-{}", std::process::id()));
        let staged = staging.join("s");
        for candidate in [&path, &staged] {
            let len = candidate.as_os_str().len();
            if len > MAX_SOCKET_PATH {
                return Err(format!(
                    "{} is {len} bytes, and a Unix socket path may have at most \
                     {MAX_SOCKET_PATH}",
                    candidate.display()
                ));
            }
        }
        let io = |what: &str, e: std::io::Error| format!("cannot {what}: {e}");
        std::fs::create_dir_all(dir).map_err(|e| io(&format!("create {}", dir.display()), e))?;
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&staging)
            .map_err(|e| io(&format!("create {}", staging.display()), e))?;
        let bound: Result<_, String> = (|| {
            let listener = std::os::unix::net::UnixListener::bind(&staged)
                .map_err(|e| io(&format!("bind {}", staged.display()), e))?;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| io(&format!("restrict {}", staged.display()), e))?;
            let meta = std::fs::symlink_metadata(&staged)
                .map_err(|e| io(&format!("read {}", staged.display()), e))?;
            std::fs::rename(&staged, &path)
                .map_err(|e| io(&format!("put the socket at {}", path.display()), e))?;
            listener
                .set_nonblocking(true)
                .map_err(|e| io("set the socket non-blocking", e))?;
            let listener = tokio::net::UnixListener::from_std(listener)
                .map_err(|e| io("listen on the socket", e))?;
            Ok((listener, (meta.dev(), meta.ino())))
        })();
        let _ = std::fs::remove_dir_all(&staging);
        let (listener, identity) = bound?;
        Ok(Self {
            path,
            identity,
            listener: Some(listener),
        })
    }

    #[cfg(not(unix))]
    pub(crate) fn open(_db_path: &str) -> Result<Self, String> {
        Err("this platform has no Unix sockets".to_string())
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The listener, once: the caller serves it and keeps `self` until the server exits.
    pub(crate) fn listener(&mut self) -> Option<tokio::net::UnixListener> {
        self.listener.take()
    }
}

impl Drop for ProjectSocket {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // A server started after this one lost its `serve.lock` would have replaced the
            // file: leave another server's socket alone.
            if std::fs::symlink_metadata(&self.path)
                .is_ok_and(|m| (m.dev(), m.ino()) == self.identity)
            {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn the_socket_is_owner_only_replaces_a_dead_one_and_is_removed_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join(".barca").join("metadata.db");
        let db = db.to_str().unwrap();
        let path = barca_service::db::serve_socket_path(db);
        // A dead server's socket: bound once, its listener gone.
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(std::os::unix::net::UnixStream::connect(&path).is_err());

        let mut socket = ProjectSocket::open(db).unwrap();
        assert_eq!(socket.path(), path);
        let mode = std::fs::symlink_metadata(&path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        let _listener = socket.listener().unwrap();
        std::os::unix::net::UnixStream::connect(&path).expect("the new server answers");
        // Nothing of the private directory it was bound in is left.
        let left: Vec<String> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, ["serve.sock"]);

        drop(socket);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_socket_another_server_put_in_place_is_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("metadata.db");
        let db = db.to_str().unwrap();
        let first = ProjectSocket::open(db).unwrap();
        let second = ProjectSocket::open(db).unwrap();
        drop(first);
        assert!(second.path().exists());
        drop(second);
    }

    #[test]
    fn a_path_too_long_for_a_socket_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("d".repeat(120)).join("metadata.db");
        let error = ProjectSocket::open(db.to_str().unwrap()).err().unwrap();
        assert!(
            error.contains("a Unix socket path may have at most"),
            "{error}"
        );
        assert!(!db.parent().unwrap().exists(), "nothing is created");
    }
}
