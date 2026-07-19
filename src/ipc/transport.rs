//! Cross-platform local IPC. Unix-domain sockets on Unix, named pipes on
//! Windows, behind one cloneable read+write `Conn` so the client/server stay
//! portable (replaces the previous `std::os::unix::net` usage). The socket is
//! still identified by a per-session filesystem path; on Windows that path is
//! hashed into a named-pipe id (pipes aren't filesystem paths).

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;

use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{ListenerOptions, Stream};

pub use interprocess::local_socket::Listener;

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

/// Bind a listener at the given per-session path (clearing a stale Unix socket).
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
        let _ = std::fs::remove_file(path);
        let name = path.to_fs_name::<GenericFilePath>()?;
        let listener = ListenerOptions::new().name(name).create_sync()?;
        // Owner-only: a connection to this socket is full command execution as
        // the user, so never rely on the umask (the state dir is also forced
        // to 0700 — see `persist::ensure_config_dir`).
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
