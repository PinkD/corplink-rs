//! Atomic secret persistence: avoid truncated files and world-readable Unix
//! defaults. Directory ownership/ACL policy belongs to the user or administrator;
//! this is not an isolation boundary against someone who can replace that directory.
use anyhow::{bail, Context, Result};
#[cfg(unix)]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

fn resolved(path: &Path, create: bool) -> Result<PathBuf> {
    let name = path
        .file_name()
        .context("secret file path must have a filename")?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if create {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(parent)?;
    }
    // Parent aliases/junctions are legitimate configuration locations. Do not
    // reject every Windows reparse point: cloud placeholders use them as well.
    Ok(parent.canonicalize()?.join(name))
}

fn check_file(metadata: &fs::Metadata) -> Result<()> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        // Atomic replacement replaces the link itself, unlike a normal write
        // through it. Require an explicit target path rather than silently
        // changing that user-visible relationship.
        bail!("secret file must be a regular file; use the target path for a symlink");
    }
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
    let mut file = options.open(&path)?;
    check_file(&file.metadata()?)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub fn write(path: &Path, data: &[u8]) -> Result<()> {
    let path = resolved(path, true)?;
    let existing = match fs::symlink_metadata(&path) {
        Ok(metadata) => {
            check_file(&metadata)?;
            Some(metadata)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let parent = path.parent().context("missing secret file parent")?;
    // A temporary file in the same directory allows an atomic replacement:
    // readers see the complete old or new contents, including when sizes differ.
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // sudo commonly reads a user's existing config from their home. Keep
        // that file's owner instead of making it unreadable to them after save.
        if let Some(metadata) = existing
            .as_ref()
            .filter(|_| unsafe { libc::geteuid() } == 0)
        {
            let result = unsafe {
                libc::fchown(
                    temporary.as_file().as_raw_fd(),
                    metadata.uid(),
                    metadata.gid(),
                )
            };
            if result != 0 {
                return Err(std::io::Error::last_os_error())
                    .context("failed to preserve secret file owner");
            }
        }
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = existing;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_is_complete_and_leaves_hardlink_contents_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config.json");
        let alias = root.path().join("config-backup.json");
        fs::write(&path, b"old-long-value").unwrap();
        fs::hard_link(&path, &alias).unwrap();
        // Exercise the reader too: merely reading must not chmod the shared inode.
        #[cfg(unix)]
        let alias_mode = {
            use std::os::unix::fs::PermissionsExt;
            fs::metadata(&alias).unwrap().permissions().mode()
        };
        assert_eq!(read(&path).unwrap(), b"old-long-value");
        write(&path, b"new").unwrap();
        assert_eq!(read(&path).unwrap(), b"new");
        assert_eq!(fs::read(&alias).unwrap(), b"old-long-value");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&alias).unwrap().permissions().mode(),
                alias_mode
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn private_modes_preserve_existing_parent_and_file_ownership() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
        let path = root.path().join("config.json");
        fs::write(&path, b"old").unwrap();
        let old = fs::metadata(&path).unwrap();
        write(&path, b"new").unwrap();
        let new = fs::metadata(&path).unwrap();
        assert_eq!(new.uid(), old.uid());
        assert_eq!(new.mode() & 0o777, 0o600);
        assert_eq!(fs::metadata(root.path()).unwrap().mode() & 0o777, 0o755);
        let nested = root.path().join("private/nested/cookies.json");
        write(&nested, b"secret").unwrap();
        for dir in [nested.parent().unwrap(), &root.path().join("private")] {
            assert_eq!(fs::metadata(dir).unwrap().mode() & 0o777, 0o700);
        }
    }

    #[cfg(unix)]
    #[test]
    fn directory_alias_is_supported_but_final_symlink_is_not_replaced() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let actual = root.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let alias = root.path().join("alias");
        symlink(&actual, &alias).unwrap();
        let target = actual.join("config.json");
        write(&alias.join("config.json"), b"secret").unwrap();
        assert_eq!(read(&target).unwrap(), b"secret");
        let link = actual.join("linked.json");
        symlink(&target, &link).unwrap();
        assert!(read(&link).is_err());
        assert!(write(&link, b"overwritten").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"secret");
    }

    #[cfg(windows)]
    #[test]
    fn directory_junction_is_a_supported_parent() {
        use std::process::Command;
        let root = tempfile::tempdir().unwrap();
        let actual = root.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let junction = root.path().join("junction");
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&actual)
            .status()
            .unwrap();
        assert!(status.success());
        let result = (|| {
            write(&junction.join("config.json"), b"secret")?;
            assert_eq!(read(&actual.join("config.json"))?, b"secret");
            Ok::<_, anyhow::Error>(())
        })();
        // Remove the junction itself before TempDir recursively cleans the fixture.
        fs::remove_dir(&junction).unwrap();
        result.unwrap();
    }
}
