use super::state::*;
use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
const RESOLV_CONF_PATH: &str = "/etc/resolv.conf";
const DEFAULT_BACKUP: &str = "resolv.conf.corplink";
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u8,
    original_device: u64,
    original_inode: u64,
    applied: String,
}

pub struct DNSManager {
    resolv: PathBuf,
    backup: PathBuf,
    state: PathBuf,
    _lock: File,
}

fn backup_filename(value: Option<String>) -> Result<String> {
    let name = value
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_BACKUP.into());
    ensure!(
        name != "."
            && name != ".."
            && name != "resolv.conf"
            && name.len() <= 180
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')),
        "dns_backup_filename must be a single basename different from resolv.conf"
    );
    Ok(name)
}

fn validate_original(meta: &fs::Metadata) -> Result<()> {
    ensure!(
        meta.is_file() || meta.file_type().is_symlink(),
        "resolv.conf must be a file or symlink"
    );
    ensure!(
        meta.uid() == unsafe { libc::geteuid() },
        "unsafe resolv.conf owner"
    );
    // Symlink permissions are not access controls; its parent is protected.
    ensure!(
        meta.file_type().is_symlink() || meta.mode() & 0o022 == 0,
        "unsafe resolv.conf write permissions"
    );
    Ok(())
}

// linkat without AT_SYMLINK_FOLLOW preserves the symlink inode itself on
// every Unix host, including when these Linux fixtures run on macOS.
fn link_original(source: &Path, backup: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = std::ffi::CString::new(source.as_os_str().as_bytes())?;
    let backup = std::ffi::CString::new(backup.as_os_str().as_bytes())?;
    let code = unsafe {
        libc::linkat(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            backup.as_ptr(),
            0,
        )
    };
    if code != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

impl DNSManager {
    #[cfg(target_os = "linux")]
    pub fn new(filename: Option<String>) -> Result<Self> {
        Self::at(Path::new(RESOLV_CONF_PATH), filename)
    }
    fn at(resolv: &Path, filename: Option<String>) -> Result<Self> {
        let name = backup_filename(filename)?;
        let dir = resolv
            .parent()
            .context("resolv.conf has no parent directory")?;
        let meta = fs::metadata(dir)?;
        ensure!(
            meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o022 == 0,
            "unsafe resolv.conf directory owner or permissions"
        );
        let lock = lock_file(&dir.join(".corplink-rs-dns.lock"))?;
        Ok(Self {
            resolv: resolv.into(),
            backup: dir.join(&name),
            state: dir.join(format!("{name}.state")),
            _lock: lock,
        })
    }

    pub fn recover_dns(&mut self) -> Result<()> {
        self.restore_dns()
    }

    pub fn set_dns(&mut self, servers: Vec<&str>, search: Vec<&str>) -> Result<()> {
        validate_servers(&servers, &search)?;
        if servers.is_empty() {
            return Ok(());
        }
        self.recover_dns()?;
        ensure!(
            fs::symlink_metadata(&self.backup)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound),
            "existing DNS backup without recovery state; refusing to overwrite it"
        );
        let original =
            fs::symlink_metadata(&self.resolv).context("failed to inspect resolv.conf")?;
        validate_original(&original)?;
        let applied = render_resolv_conf(&servers, &search);
        let journal = Journal {
            version: 1,
            original_device: original.dev(),
            original_inode: original.ino(),
            applied,
        };
        let temp = self.resolv.parent().unwrap().join(format!(
            ".corplink-resolv-{:016x}.tmp",
            rand::random::<u64>()
        ));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o644)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&temp)?;
            file.write_all(journal.applied.as_bytes())?;
            file.sync_all()?;
            // hard_link fails if backup exists and preserves a symlink itself;
            // never follow resolv.conf or overwrite an unbacked-up original.
            link_original(&self.resolv, &self.backup)
                .context("failed to create DNS backup; resolv.conf was not changed")?;
            let backup_meta = fs::symlink_metadata(&self.backup)?;
            if backup_meta.dev() != journal.original_device
                || backup_meta.ino() != journal.original_inode
            {
                let _ = fs::remove_file(&self.backup);
                anyhow::bail!(
                    "resolv.conf changed while creating its backup; no override installed"
                );
            }
            if let Err(error) = write_state(&self.state, &journal) {
                let _ = fs::remove_file(&self.backup);
                return Err(error);
            }
            fs::rename(&temp, &self.resolv).context("failed to install DNS settings")?;
            File::open(self.resolv.parent().unwrap())?.sync_all()?;
            Ok(())
        })();
        let _ = fs::remove_file(&temp);
        if let Err(error) = result {
            return match self.restore_dns() {
                Ok(()) => Err(error),
                Err(restore) => {
                    Err(error).context(format!("DNS recovery remains pending: {restore:#}"))
                }
            };
        }
        Ok(())
    }

    pub fn restore_dns(&mut self) -> Result<()> {
        let Some(journal) = read_state::<Journal>(&self.state)? else {
            return Ok(());
        };
        ensure!(
            journal.version == 1 && journal.applied.starts_with("# Generated by corplink-rs\n"),
            "invalid DNS recovery state"
        );
        let current =
            fs::symlink_metadata(&self.resolv).context("failed to inspect current resolv.conf")?;
        let original_still_installed =
            current.dev() == journal.original_device && current.ino() == journal.original_inode;
        match fs::symlink_metadata(&self.backup) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && original_still_installed => {
                // Recovery completed before a crash, but journal removal did not.
                return remove_state(&self.state);
            }
            Err(e) => return Err(e).context("failed to inspect DNS backup"),
            Ok(backup) => {
                validate_original(&backup)?;
                ensure!(
                    backup.dev() == journal.original_device
                        && backup.ino() == journal.original_inode,
                    "DNS backup identity changed; refusing restoration"
                );
            }
        }
        if original_still_installed {
            // Crash before override. The original is already in place.
            fs::remove_file(&self.backup).context("failed to remove unused DNS backup")?;
        } else {
            validate_original(&current)?;
            ensure!(
                current.is_file(),
                "resolv.conf changed externally; DNS backup retained for manual recovery"
            );
            let mut file = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&self.resolv)?;
            ensure!(
                file.metadata()?.len() <= 1024 * 1024,
                "resolv.conf changed externally; backup retained"
            );
            let mut content = String::new();
            file.read_to_string(&mut content)?;
            ensure!(
                content == journal.applied,
                "resolv.conf changed externally; DNS backup retained for manual recovery"
            );
            fs::rename(&self.backup, &self.resolv)
                .context("failed to restore resolv.conf; backup retained")?;
        }
        remove_state(&self.state)
    }
}

fn render_resolv_conf(servers: &[&str], search: &[&str]) -> String {
    let mut out = String::from("# Generated by corplink-rs\n");
    for dns in servers {
        out.push_str(&format!("nameserver {dns}\n"));
    }
    if !search.is_empty() {
        out.push_str(&format!("search {}\n", search.join(" ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::state::test_support::TempDir;
    fn fixture() -> (TempDir, PathBuf) {
        let temp = TempDir::new();
        let resolv = temp.0.join("resolv.conf");
        fs::write(&resolv, "nameserver 1.1.1.1\n").unwrap();
        (temp, resolv)
    }

    #[test]
    fn backup_filename_is_a_single_basename() {
        for name in [
            "../passwd",
            "/etc/passwd",
            "..",
            ".",
            "resolv.conf",
            "a/b",
            "a\\b",
            "a\n",
        ] {
            assert!(backup_filename(Some(name.into())).is_err());
        }
        assert_eq!(backup_filename(None).unwrap(), DEFAULT_BACKUP);
        assert_eq!(
            backup_filename(Some("custom.bak".into())).unwrap(),
            "custom.bak"
        );
    }

    #[test]
    fn recovery_preserves_original_file_and_symlink() {
        for symlink in [false, true] {
            let (temp, resolv) = fixture();
            if symlink {
                fs::rename(&resolv, temp.0.join("original")).unwrap();
                std::os::unix::fs::symlink("original", &resolv).unwrap();
            }
            {
                let mut manager = DNSManager::at(&resolv, None).unwrap();
                manager
                    .set_dns(vec!["10.0.0.53"], vec!["corp.example"])
                    .unwrap();
                assert!(fs::read_to_string(&resolv)
                    .unwrap()
                    .contains("nameserver 10.0.0.53"));
                assert_eq!(fs::metadata(&manager.state).unwrap().mode() & 0o777, 0o600);
            }
            let mut restarted = DNSManager::at(&resolv, None).unwrap();
            restarted.recover_dns().unwrap();
            assert_eq!(fs::read_to_string(&resolv).unwrap(), "nameserver 1.1.1.1\n");
            assert_eq!(
                fs::symlink_metadata(&resolv)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                symlink
            );
        }
    }

    #[test]
    fn existing_backup_and_backup_failure_never_overwrite_dns() {
        let (_temp, resolv) = fixture();
        let mut manager = DNSManager::at(&resolv, None).unwrap();
        fs::write(&manager.backup, "retain me").unwrap();
        assert!(manager.set_dns(vec!["10.0.0.53"], vec![]).is_err());
        assert_eq!(fs::read_to_string(&resolv).unwrap(), "nameserver 1.1.1.1\n");
        fs::remove_file(&manager.backup).unwrap();
        // Nonexistent parent deterministically fails backup creation, even as root.
        manager.backup = resolv.parent().unwrap().join("absent/backup");
        assert!(manager.set_dns(vec!["10.0.0.53"], vec![]).is_err());
        assert_eq!(fs::read_to_string(&resolv).unwrap(), "nameserver 1.1.1.1\n");
    }

    #[test]
    fn external_edits_and_restore_failure_preserve_backup_and_report_error() {
        let (_temp, resolv) = fixture();
        let mut manager = DNSManager::at(&resolv, None).unwrap();
        manager.set_dns(vec!["10.0.0.53"], vec![]).unwrap();
        fs::write(&resolv, "nameserver 9.9.9.9\n").unwrap();
        assert!(manager.restore_dns().is_err());
        assert!(manager.backup.exists());
        assert_eq!(fs::read_to_string(&resolv).unwrap(), "nameserver 9.9.9.9\n");
        fs::remove_file(&resolv).unwrap();
        fs::create_dir(&resolv).unwrap();
        assert!(manager.restore_dns().is_err());
        assert!(manager.backup.exists());
    }

    #[test]
    fn unsafe_state_and_dns_injection_are_rejected() {
        let (temp, resolv) = fixture();
        let mut manager = DNSManager::at(&resolv, None).unwrap();
        assert!(manager
            .set_dns(vec!["1.1.1.1\nsearch attacker.example"], vec![])
            .is_err());
        std::os::unix::fs::symlink(temp.0.join("missing"), &manager.state).unwrap();
        assert!(manager.recover_dns().is_err());
        assert_eq!(fs::read_to_string(&resolv).unwrap(), "nameserver 1.1.1.1\n");
    }
    #[test]
    fn different_backup_names_cannot_bypass_lock_and_changed_backups_are_rejected() {
        let (_temp, resolv) = fixture();
        let mut manager = DNSManager::at(&resolv, None).unwrap();
        assert!(DNSManager::at(&resolv, Some("different.bak".into())).is_err());
        manager.set_dns(vec!["10.0.0.53"], vec![]).unwrap();
        let replacement = resolv.parent().unwrap().join("replacement");
        fs::write(&replacement, "unrelated file").unwrap();
        fs::rename(&replacement, &manager.backup).unwrap();
        assert!(manager.restore_dns().is_err());
        assert!(fs::read_to_string(&resolv).unwrap().contains("10.0.0.53"));
        assert!(manager.state.exists());
    }
}
