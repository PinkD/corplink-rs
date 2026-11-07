//! System DNS is opt-in. Recovery state contains DNS settings only and is held
//! under an exclusive lock until the manager is dropped.
pub(super) use anyhow::{ensure, Context, Result};
pub(super) use std::net::IpAddr;

#[cfg(all(test, unix))]
pub(super) use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
pub(super) use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
#[cfg(any(target_os = "macos", target_os = "linux", test))]
pub(super) use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub(super) fn validate_servers(servers: &[&str], search: &[&str]) -> Result<()> {
    for server in servers {
        server
            .parse::<IpAddr>()
            .context("invalid DNS server address")?;
    }
    for domain in search {
        let domain = domain.strip_suffix('.').unwrap_or(domain);
        ensure!(
            !domain.is_empty()
                && domain.len() <= 253
                && domain.split('.').all(|part| {
                    !part.is_empty()
                        && part.len() <= 63
                        && !part.starts_with('-')
                        && !part.ends_with('-')
                        && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                }),
            "invalid DNS search domain"
        );
    }
    Ok(())
}

#[cfg(unix)]
pub(super) fn check_private_file(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.nlink() == 1,
        "DNS recovery file must be a regular file with one link"
    );
    ensure!(
        meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
        "DNS recovery file has unsafe owner or permissions"
    );
    Ok(())
}

#[cfg(unix)]
pub(super) fn open_private(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .with_context(|| format!("failed to open DNS recovery file {}", path.display()))?;
    check_private_file(&file)?;
    Ok(file)
}

#[cfg(unix)]
pub(super) fn lock_file(path: &Path) -> Result<File> {
    let file = open_private(path, true)?;
    use std::os::fd::AsRawFd;
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0,
        "another process is managing DNS; recovery state is locked"
    );
    Ok(file)
}

#[cfg(unix)]
pub(super) fn read_state<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(_) => (),
    }
    let mut file = open_private(path, false)?;
    ensure!(
        file.metadata()?.len() <= 1024 * 1024,
        "DNS recovery state is too large"
    );
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    serde_json::from_slice(&bytes)
        .context("invalid DNS recovery state")
        .map(Some)
}

#[cfg(unix)]
pub(super) fn write_state<T: serde::Serialize>(path: &Path, state: &T) -> Result<()> {
    let parent = path.parent().context("missing recovery state directory")?;
    let temp = parent.join(format!(".corplink-dns-{:016x}.tmp", rand::random::<u64>()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&temp)?;
        file.write_all(&serde_json::to_vec(state)?)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(unix)]
pub(super) fn remove_state(path: &Path) -> Result<()> {
    fs::remove_file(path).context("failed to remove completed DNS recovery state")?;
    File::open(path.parent().context("missing state directory")?)?.sync_all()?;
    Ok(())
}

#[cfg(all(test, unix))]
pub(super) mod test_support {
    use super::*;
    pub struct TempDir(pub PathBuf);
    impl TempDir {
        pub fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("corplink-dns-test-{:016x}", rand::random::<u64>()));
            fs::create_dir(&dir).unwrap();
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
