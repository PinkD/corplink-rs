//! Secret persistence. Unix files are replaced atomically with mode 0600;
//! newly created directories use 0700. Existing user directories are never chmod'd.
//! Windows does not have Unix mode bits: administrators must provision a private
//! directory ACL. This module rejects reparse points, but does not install a DACL.
use anyhow::{bail, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(unix)]
fn trusted_directory_owner(owner: u32, current_user: u32) -> bool {
    owner == current_user || owner == 0
}

fn check_directory(path: &Path, metadata: &fs::Metadata) -> Result<()> {
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        bail!(
            "secret file directory must be a real directory: {}",
            path.display()
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let uid = unsafe { libc::geteuid() };
        // root may not write into a directory controlled by an unprivileged user.
        if !trusted_directory_owner(metadata.uid(), uid) {
            bail!(
                "untrusted owner of secret file directory: {}",
                path.display()
            );
        }
        // /tmp is safe as an ancestor because sticky, root-owned directories
        // protect entries owned by other users. Every child is checked separately.
        let trusted_sticky = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
        if metadata.mode() & 0o022 != 0 && !trusted_sticky {
            bail!(
                "secret file directory is writable by other users: {}",
                path.display()
            );
        }
    }
    #[cfg(windows)]
    reject_reparse(metadata)?;
    Ok(())
}

#[cfg(windows)]
fn reject_reparse(metadata: &fs::Metadata) -> Result<()> {
    use std::os::windows::fs::MetadataExt;
    if metadata.file_attributes() & 0x400 != 0 {
        bail!("secret paths must not be Windows reparse points");
    }
    Ok(())
}

fn directory(path: &Path, create: bool) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    for ancestor in absolute.ancestors() {
        if let Ok(metadata) = fs::symlink_metadata(ancestor) {
            if metadata.file_type().is_symlink() {
                // macOS exposes these root-owned operating-system aliases.
                #[cfg(target_os = "macos")]
                {
                    use std::os::unix::fs::MetadataExt;
                    if metadata.uid() == 0
                        && [Path::new("/tmp"), Path::new("/var"), Path::new("/etc")]
                            .contains(&ancestor)
                    {
                        continue;
                    }
                }
                bail!(
                    "secret file directory must not be a symlink: {}",
                    ancestor.display()
                );
            }
            #[cfg(windows)]
            reject_reparse(&metadata)?;
        }
    }
    // Resolve OS aliases such as macOS /var and /tmp, then check every canonical
    // ancestor. Never operate through the original symlink path afterward.
    let canonical = match absolute.canonicalize() {
        Ok(path) => path,
        Err(e) if create && e.kind() == std::io::ErrorKind::NotFound => {
            let parent = directory(absolute.parent().context("missing directory parent")?, true)?;
            let name = absolute.file_name().context("invalid directory name")?;
            let child = parent.join(name);
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&child) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
            check_directory(&child, &fs::symlink_metadata(&child)?)?;
            child
        }
        Err(e) => return Err(e.into()),
    };
    for ancestor in canonical.ancestors() {
        check_directory(ancestor, &fs::symlink_metadata(ancestor)?)?;
    }
    Ok(canonical)
}

fn resolved(path: &Path, create: bool) -> Result<PathBuf> {
    let name = path
        .file_name()
        .context("secret file path must have a filename")?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(directory(parent, create)?.join(name))
}

fn check_file(metadata: &fs::Metadata) -> Result<()> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("secret file must be a regular file, not a symlink");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } {
            bail!("secret file is not owned by the current user");
        }
        if metadata.nlink() != 1 {
            bail!("secret file must not have multiple hard links");
        }
    }
    #[cfg(windows)]
    reject_reparse(metadata)?;
    Ok(())
}

pub fn read(path: &Path) -> Result<Vec<u8>> {
    let path = resolved(path, false)?;
    check_file(&fs::symlink_metadata(&path)?)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x00200000);
    }
    let mut file = options.open(&path)?;
    check_file(&file.metadata()?)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn write(path: &Path, data: &[u8]) -> Result<()> {
    let path = resolved(path, true)?;
    match fs::symlink_metadata(&path) {
        Ok(metadata) => check_file(&metadata)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let parent = path.parent().context("missing secret file parent")?;
    // tempfile uses create_new and mode 0600 on Unix. Rename never follows a
    // destination symlink and readers see either the complete old or new file.
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(data)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .map_err(|e| e.error)
        .context("failed to atomically replace secret file")?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};

    #[test]
    fn atomic_replacement_is_private_and_preserves_parent_mode() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let path = root.path().join("config.json");
        fs::write(&path, b"old-long-value").unwrap();
        let old = File::open(&path).unwrap();
        write(&path, b"new").unwrap();
        assert_eq!(read(&path).unwrap(), b"new");
        assert_ne!(
            old.metadata().unwrap().ino(),
            fs::metadata(&path).unwrap().ino()
        );
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(root.path()).unwrap().mode() & 0o777, 0o755);
        let nested = root.path().join("private/nested/cookies.json");
        write(&nested, b"secret").unwrap();
        assert_eq!(
            fs::metadata(nested.parent().unwrap()).unwrap().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.path().join("private")).unwrap().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn symlinks_hardlinks_and_untrusted_directories_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("target");
        fs::write(&target, b"untouched").unwrap();
        let link = root.path().join("link");
        symlink(&target, &link).unwrap();
        assert!(read(&link).is_err());
        assert!(write(&link, b"overwritten").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"untouched");
        let hard = root.path().join("hard");
        fs::hard_link(&target, &hard).unwrap();
        assert!(write(&hard, b"overwritten").is_err());
        let unsafe_dir = root.path().join("unsafe");
        fs::create_dir(&unsafe_dir).unwrap();
        fs::set_permissions(&unsafe_dir, fs::Permissions::from_mode(0o777)).unwrap();
        assert!(write(&unsafe_dir.join("secret"), b"secret").is_err());
        assert!(!unsafe_dir.join("secret").exists());
        let linked_dir = root.path().join("directory-link");
        symlink(root.path(), &linked_dir).unwrap();
        assert!(write(&linked_dir.join("secret"), b"secret").is_err());
    }

    #[test]
    fn root_never_trusts_an_unprivileged_directory_owner() {
        assert!(!trusted_directory_owner(501, 0));
        assert!(!trusted_directory_owner(502, 501));
        assert!(trusted_directory_owner(501, 501));
        assert!(trusted_directory_owner(0, 501));
    }
}
