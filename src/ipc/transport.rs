//! Cross-platform local IPC. Unix-domain sockets on Unix, named pipes on
//! Windows, behind one cloneable read+write `Conn` so the client/server stay
//! portable (replaces the previous `std::os::unix::net` usage). The socket is
//! still identified by a per-session filesystem path; on Windows that path is
//! hashed into a named-pipe id (pipes aren't filesystem paths).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;

use fs2::FileExt;
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{ListenerOptions, Stream};

pub use interprocess::local_socket::Listener;

/// Exclusive, process-scoped guard for creating Bohay's two server sockets.
///
/// The lock file remains on disk after the holder exits; the OS releases the
/// advisory lock automatically, including after a crash. Keeping the file
/// avoids a second race around creating and deleting a lock pathname.
pub struct ServerStartupLock {
    _file: File,
}

/// Acquire exclusive ownership of server startup for one Bohay state directory.
/// Hold the returned guard while checking, reclaiming, and binding both sockets.
pub fn acquire_server_startup_lock(state_dir: &Path) -> io::Result<ServerStartupLock> {
    fs::create_dir_all(state_dir)?;
    let path = state_dir.join("server.lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)?;
    file.lock_exclusive()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(ServerStartupLock { _file: file })
}

impl ServerStartupLock {
    /// Remove a socket only after its listener is proven unreachable. A live
    /// socket is never replaced: doing so would orphan its server process.
    pub fn reclaim_stale_socket(&self, path: &Path) -> io::Result<()> {
        #[cfg(windows)]
        {
            let _ = path;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            use std::os::unix::fs::FileTypeExt;

            let metadata = match fs::symlink_metadata(path) {
                Ok(metadata) => metadata,
                Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(err) => return Err(err),
            };
            if !metadata.file_type().is_socket() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("refusing to replace non-socket path {}", path.display()),
                ));
            }
            if connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("a Bohay listener is already active at {}", path.display()),
                ));
            }
            fs::remove_file(path)
        }
    }
}

/// A cloneable owned read+write handle to one connection — the portable
/// stand-in for a cloned `UnixStream`. Clones share the full-duplex socket, so
/// one clone can read while another writes (as `try_clone` did before).
#[derive(Clone)]
pub struct Conn(Arc<Stream>);

impl Conn {
    fn new(stream: Stream) -> Self {
        Conn(Arc::new(stream))
    }
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(buf)
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self.0).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}

#[cfg(windows)]
fn pipe_id(path: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    format!("bohay-{:016x}", h.finish())
}

/// Connect to a server socket identified by a per-session filesystem path.
pub fn connect(path: &Path) -> io::Result<Conn> {
    #[cfg(windows)]
    {
        use interprocess::local_socket::GenericNamespaced;
        let id = pipe_id(path);
        let name = id.to_ns_name::<GenericNamespaced>()?;
        Ok(Conn::new(Stream::connect(name)?))
    }
    #[cfg(not(windows))]
    {
        use interprocess::local_socket::GenericFilePath;
        let name = path.to_fs_name::<GenericFilePath>()?;
        Ok(Conn::new(Stream::connect(name)?))
    }
}

/// Bind a listener at the given per-session path.
///
/// Call [`ServerStartupLock::reclaim_stale_socket`] first while holding the
/// state-directory startup lock. This function never removes an existing path.
pub fn bind(path: &Path) -> io::Result<Listener> {
    #[cfg(windows)]
    {
        use interprocess::local_socket::GenericNamespaced;
        let id = pipe_id(path);
        let name = id.to_ns_name::<GenericNamespaced>()?;
        ListenerOptions::new().name(name).create_sync()
    }
    #[cfg(not(windows))]
    {
        use interprocess::local_socket::GenericFilePath;
        let name = path.to_fs_name::<GenericFilePath>()?;
        let listener = ListenerOptions::new().name(name).create_sync()?;
        // Owner-only: a connection to this socket is full command execution as
        // the user, so never rely on the umask (the selected session dir is also
        // forced to 0700 — see `persist::ensure_session_dir`).
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(listener)
    }
}

/// Iterate accepted connections (errors skipped), as `Conn`s.
pub fn incoming(listener: &Listener) -> impl Iterator<Item = Conn> + '_ {
    listener.incoming().flatten().map(Conn::new)
}

#[cfg(all(test, unix))]
mod tests {
    use super::{acquire_server_startup_lock, ServerStartupLock};
    use std::io;
    use std::os::unix::net::UnixListener;

    fn test_socket(
        name: &str,
    ) -> (
        crate::persist::TestEnv,
        ServerStartupLock,
        std::path::PathBuf,
    ) {
        let env = crate::persist::test_env(name);
        let dir = crate::persist::ensure_config_dir();
        let lock = acquire_server_startup_lock(&dir).unwrap();
        (env, lock, dir.join("bohay.sock"))
    }

    #[test]
    fn live_socket_is_never_reclaimed() {
        let (_env, lock, path) = test_socket("live-socket");
        let _listener = UnixListener::bind(&path).unwrap();

        let err = lock.reclaim_stale_socket(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert!(path.exists(), "a live socket pathname must remain in place");
    }

    #[test]
    fn stale_socket_is_reclaimed_while_holding_startup_lock() {
        let (_env, lock, path) = test_socket("stale-socket");
        let listener = UnixListener::bind(&path).unwrap();
        drop(listener);
        assert!(path.exists(), "dropping a UnixListener leaves a stale path");

        lock.reclaim_stale_socket(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn non_socket_path_is_not_deleted() {
        let (_env, lock, path) = test_socket("non-socket");
        std::fs::write(&path, "do not delete").unwrap();

        let err = lock.reclaim_stale_socket(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "do not delete");
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::pipe_id;
    use std::path::Path;

    #[test]
    fn named_session_paths_derive_distinct_stable_pipe_ids() {
        let alpha = pipe_id(Path::new(r"C:\Users\riz\.bohay\sessions\alpha\bohay.sock"));
        let beta = pipe_id(Path::new(r"C:\Users\riz\.bohay\sessions\beta\bohay.sock"));
        assert_ne!(alpha, beta);
        assert_eq!(
            alpha,
            pipe_id(Path::new(r"C:\Users\riz\.bohay\sessions\alpha\bohay.sock"))
        );
    }
}
