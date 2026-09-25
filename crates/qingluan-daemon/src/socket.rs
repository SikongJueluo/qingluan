//! Secure Unix-domain socket lifecycle.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use tokio::net::UnixListener;

const UNIX_PATH_MAX: usize = 107;

pub struct BoundUnixSocket {
    listener: UnixListener,
    guard: SocketGuard,
}

impl BoundUnixSocket {
    pub fn bind(path: &Path) -> io::Result<Self> {
        validate_socket_path(path)?;
        prepare_private_parent(path)?;
        let lock = acquire_process_lock(path)?;
        prepare_socket_path(path)?;
        let listener = UnixListener::bind(path)?;
        // The parent is already owner-only and the process lock serializes
        // daemon starters, so chmod can safely close the umask-created mode.
        if let Err(error) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
            let _ = std::fs::remove_file(path);
            return Err(error);
        }
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) => {
                let _ = std::fs::remove_file(path);
                return Err(error);
            }
        };
        if !metadata.file_type().is_socket() || metadata.permissions().mode() & 0o777 != 0o600 {
            let _ = std::fs::remove_file(path);
            return Err(io::Error::other("bound terminal socket is not mode 0600"));
        }
        Ok(Self {
            listener,
            guard: SocketGuard {
                path: path.to_owned(),
                device: metadata.dev(),
                inode: metadata.ino(),
                _lock: lock,
            },
        })
    }

    pub fn into_parts(self) -> (UnixListener, SocketGuard) {
        (self.listener, self.guard)
    }
}

/// Keeps the process lock and removes only the exact socket inode this
/// instance bound. It never unlinks a replacement created by another owner.
pub struct SocketGuard {
    path: PathBuf,
    device: u64,
    inode: u64,
    _lock: File,
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let Ok(metadata) = std::fs::symlink_metadata(&self.path) else {
            return;
        };
        if metadata.file_type().is_socket()
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn validate_socket_path(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal socket path must be absolute",
        ));
    }
    if path.as_os_str().as_bytes().len() > UNIX_PATH_MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal socket path exceeds the Unix sun_path limit",
        ));
    }
    if path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "terminal socket path has no file name",
        ));
    }
    Ok(())
}

fn prepare_private_parent(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "terminal socket has no parent")
    })?;
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() {
                return Err(io::Error::other(format!(
                    "terminal socket parent is not a directory: {}",
                    parent.display()
                )));
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::create_dir_all(parent)?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
        Err(error) => return Err(error),
    }
    let metadata = std::fs::symlink_metadata(parent)?;
    // SAFETY: geteuid has no preconditions and does not access memory.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "terminal socket parent is not owned by the current user: {}",
                parent.display()
            ),
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "terminal socket parent must not be accessible by group or other: {}",
                parent.display()
            ),
        ));
    }
    Ok(())
}

fn lock_path(socket: &Path) -> PathBuf {
    let mut name = socket
        .file_name()
        .expect("validated socket file name")
        .to_os_string();
    name.push(".lock");
    socket.with_file_name(name)
}

fn acquire_process_lock(socket: &Path) -> io::Result<File> {
    let path = lock_path(socket);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(&path)?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no preconditions and does not access memory.
    let effective_uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_file() || metadata.uid() != effective_uid || metadata.nlink() != 1 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("unsafe terminal daemon lock file: {}", path.display()),
        ));
    }
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    // SAFETY: flock operates on the live descriptor owned by `file`.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(file);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        // Preserve the Gate A operational classification when the current
        // owner has already bound the socket.
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return Err(io::Error::other(format!(
                "refusing to unlink active socket: {}",
                socket.display()
            )));
        }
        return Err(io::Error::other(format!(
            "refusing to start while terminal daemon lock is held: {}",
            path.display()
        )));
    }
    Err(error)
}

/// Make the path safe to bind without ever replacing a non-socket or an
/// active listener. A dead listener's connection-refused inode is stale and
/// may be removed after the process lock has been acquired.
fn prepare_socket_path(path: &Path) -> io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() {
        return Err(io::Error::other(format!(
            "refusing to replace non-socket path: {}",
            path.display()
        )));
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(io::Error::other(format!(
            "refusing to unlink active socket: {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
            let current = std::fs::symlink_metadata(path)?;
            if !current.file_type().is_socket()
                || current.dev() != metadata.dev()
                || current.ino() != metadata.ino()
            {
                return Err(io::Error::other(format!(
                    "socket path changed while checking staleness: {}",
                    path.display()
                )));
            }
            std::fs::remove_file(path)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "qingluan-s6-socket-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn regular_file_is_rejected_and_preserved() {
        let root = TempDir::new("regular");
        let path = root.0.join("daemon.sock");
        let mut file = File::create(&path).unwrap();
        file.write_all(b"keep-me").unwrap();
        drop(file);
        let error = match BoundUnixSocket::bind(&path) {
            Ok(_) => panic!("regular path unexpectedly replaced"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("refusing to replace non-socket path")
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"keep-me");
    }

    #[tokio::test]
    async fn symlink_socket_path_is_rejected_and_preserved() {
        let root = TempDir::new("symlink");
        let target = root.0.join("target");
        let path = root.0.join("daemon.sock");
        std::fs::write(&target, b"keep-me").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        let error = match BoundUnixSocket::bind(&path) {
            Ok(_) => panic!("symlink unexpectedly replaced"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("refusing to replace non-socket path")
        );
        assert!(
            std::fs::symlink_metadata(&path)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"keep-me");
    }

    #[tokio::test]
    async fn active_socket_is_rejected_and_exact_inode_is_cleaned() {
        let root = TempDir::new("active");
        let path = root.0.join("daemon.sock");
        let first = BoundUnixSocket::bind(&path).unwrap();
        let before = std::fs::symlink_metadata(&path).unwrap();
        let error = match BoundUnixSocket::bind(&path) {
            Ok(_) => panic!("second listener unexpectedly bound"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("refusing to unlink active socket")
        );
        let after = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
        drop(first);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn concurrent_starters_leave_exactly_one_owned_socket() {
        let root = TempDir::new("concurrent");
        let path = root.0.join("daemon.sock");
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(3));
        let spawn = |path: PathBuf, barrier: std::sync::Arc<tokio::sync::Barrier>| {
            tokio::spawn(async move {
                barrier.wait().await;
                BoundUnixSocket::bind(&path)
            })
        };
        let first = spawn(path.clone(), barrier.clone());
        let second = spawn(path.clone(), barrier.clone());
        barrier.wait().await;
        let (first, second) = tokio::join!(first, second);
        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        let error = first
            .as_ref()
            .err()
            .or_else(|| second.as_ref().err())
            .unwrap();
        assert!(
            error.to_string().contains("active socket")
                || error.to_string().contains("daemon lock is held")
        );
        assert!(path.exists());
        drop(first);
        drop(second);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn stale_socket_is_reclaimed_and_mode_is_private() {
        let root = TempDir::new("stale");
        let path = root.0.join("daemon.sock");
        let stale = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(stale);
        assert!(path.exists());
        let bound = BoundUnixSocket::bind(&path).unwrap();
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        drop(bound);
        assert!(!path.exists());
    }

    #[test]
    fn unsafe_parent_and_overlong_path_are_rejected() {
        let root = TempDir::new("unsafe-parent");
        std::fs::set_permissions(&root.0, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = match BoundUnixSocket::bind(&root.0.join("daemon.sock")) {
            Ok(_) => panic!("unsafe parent unexpectedly accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);

        let overlong = PathBuf::from(format!("/tmp/{}", "x".repeat(200)));
        let error = match BoundUnixSocket::bind(&overlong) {
            Ok(_) => panic!("overlong path unexpectedly accepted"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
