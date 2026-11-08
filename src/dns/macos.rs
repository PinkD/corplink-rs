use super::state::*;
use serde::{Deserialize, Serialize};
#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(target_os = "macos")]
const STATE_DIR: &str = "/var/db/corplink-rs";
type Runner = Box<dyn FnMut(&[String]) -> Result<String>>;

#[derive(Clone, Copy, Serialize, Deserialize)]
enum Setting {
    Servers,
    Search,
}
impl Setting {
    fn get(self) -> &'static str {
        match self {
            Self::Servers => "-getdnsservers",
            Self::Search => "-getsearchdomains",
        }
    }
    fn set(self) -> &'static str {
        match self {
            Self::Servers => "-setdnsservers",
            Self::Search => "-setsearchdomains",
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Change {
    service: String,
    setting: Setting,
    original: Vec<String>,
    applied: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    version: u8,
    changes: Vec<Change>,
}

pub struct DNSManager {
    state: PathBuf,
    _lock: File,
    run: Runner,
}

#[cfg(target_os = "macos")]
fn run_networksetup(args: &[String]) -> Result<String> {
    let output = Command::new("/usr/sbin/networksetup")
        .args(args)
        .output()
        .context("failed to execute networksetup")?;
    // Do not include command output: it may expose configuration or be untrusted.
    ensure!(
        output.status.success(),
        "networksetup {} failed ({})",
        args.first().map(String::as_str).unwrap_or(""),
        output.status
    );
    String::from_utf8(output.stdout).context("networksetup returned invalid UTF-8")
}

fn validate_values(setting: Setting, values: &[String]) -> Result<()> {
    ensure!(!values.is_empty(), "empty DNS recovery value");
    if values == ["Empty"] {
        return Ok(());
    }
    let refs: Vec<_> = values.iter().map(String::as_str).collect();
    match setting {
        Setting::Servers => validate_servers(&refs, &[]),
        Setting::Search => validate_servers(&[], &refs),
    }
}

impl DNSManager {
    #[cfg(target_os = "macos")]
    pub fn new(_: Option<String>) -> Result<Self> {
        Self::at(Path::new(STATE_DIR), Box::new(run_networksetup))
    }

    fn at(dir: &Path, run: Runner) -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        match fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e).context("failed to create private DNS recovery directory"),
        }
        let meta = fs::symlink_metadata(dir)?;
        ensure!(
            meta.is_dir() && meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0,
            "unsafe DNS recovery directory owner, permissions, or symlink"
        );
        let lock = lock_file(&dir.join("dns.lock"))?;
        Ok(Self {
            state: dir.join("dns.json"),
            _lock: lock,
            run,
        })
    }

    fn read_setting(&mut self, service: &str, setting: Setting) -> Result<Vec<String>> {
        let output = (self.run)(&[setting.get().into(), service.into()])?;
        let value = output.trim();
        let values = if value.starts_with("There aren't any DNS Servers set on ")
            || value.starts_with("There aren't any Search Domains set on ")
        {
            vec!["Empty".to_owned()]
        } else {
            value.lines().map(str::to_owned).collect()
        };
        validate_values(setting, &values)?;
        Ok(values)
    }

    fn write_setting(&mut self, change: &Change, values: &[String]) -> Result<()> {
        let mut args = vec![change.setting.set().into(), change.service.clone()];
        args.extend_from_slice(values);
        (self.run)(&args)?;
        Ok(())
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
        let services = (self.run)(&["-listallnetworkservices".into()])?;
        let mut changes = Vec::new();
        for service in services.lines().skip(1).map(str::trim) {
            // Asterisk means disabled; never enable or alter those services.
            if service.is_empty() || service.starts_with('*') {
                continue;
            }
            ensure!(
                !service.chars().any(char::is_control),
                "invalid network service name"
            );
            for (setting, values) in [(Setting::Servers, &servers), (Setting::Search, &search)] {
                if values.is_empty() {
                    continue;
                }
                changes.push(Change {
                    service: service.into(),
                    setting,
                    original: self.read_setting(service, setting)?,
                    applied: values.iter().map(|s| (*s).to_owned()).collect(),
                });
            }
        }
        if changes.is_empty() {
            return Ok(());
        }
        let journal = Journal {
            version: 1,
            changes,
        };
        // Persist intent before the first mutation. Recovery compares current
        // values, so a crash before a setter is safe as well as one after it.
        write_state(&self.state, &journal)?;
        for change in &journal.changes {
            if let Err(error) = self.write_setting(change, &change.applied) {
                return match self.restore_dns() {
                    Ok(()) => Err(error).context("DNS setup failed; previous settings restored"),
                    Err(rollback) => Err(error).context(format!(
                        "DNS setup failed; recovery remains pending: {rollback:#}"
                    )),
                };
            }
        }
        Ok(())
    }

    pub fn restore_dns(&mut self) -> Result<()> {
        let Some(journal) = read_state::<Journal>(&self.state)? else {
            return Ok(());
        };
        ensure!(journal.version == 1, "unsupported DNS recovery version");
        // Validate the entire journal before issuing any commands.
        for change in &journal.changes {
            ensure!(
                !change.service.is_empty()
                    && !change.service.starts_with('*')
                    && !change.service.chars().any(char::is_control),
                "invalid recovery service name"
            );
            validate_values(change.setting, &change.original)?;
            validate_values(change.setting, &change.applied)?;
        }
        let services = (self.run)(&["-listallnetworkservices".into()])?;
        let enabled: std::collections::HashSet<_> = services
            .lines()
            .skip(1)
            .map(str::trim)
            .filter(|name| !name.is_empty() && !name.starts_with('*'))
            .collect();
        let mut errors = Vec::new();
        for change in &journal.changes {
            if !enabled.contains(change.service.as_str()) {
                // Keep its original value durably: disabling a service must
                // neither change its settings nor discard its recovery record.
                errors.push(format!(
                    "{} is removed or disabled; recovery remains pending",
                    change.service
                ));
                continue;
            }
            let result = (|| {
                let current = self.read_setting(&change.service, change.setting)?;
                if current == change.applied {
                    self.write_setting(change, &change.original)?;
                } else if current != change.original {
                    log::warn!(
                        "DNS settings for {} changed externally; preserving them",
                        change.service
                    );
                }
                Ok::<_, anyhow::Error>(())
            })();
            if let Err(error) = result {
                errors.push(format!("{}: {error:#}", change.service));
            }
        }
        ensure!(
            errors.is_empty(),
            "DNS recovery incomplete: {}",
            errors.join("; ")
        );
        remove_state(&self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dns::state::test_support::TempDir;
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    #[derive(Default)]
    struct Network {
        values: HashMap<String, Vec<String>>,
        calls: Vec<Vec<String>>,
        fail_set: Option<String>,
        wifi_disabled: bool,
    }
    fn mock(network: Arc<Mutex<Network>>) -> Runner {
        Box::new(move |args| {
            let mut net = network.lock().unwrap();
            net.calls.push(args.to_vec());
            match args[0].as_str() {
                "-listallnetworkservices" => Ok(format!(
                    "An asterisk (*) denotes a disabled service.\n{}Wi-Fi\n*Disabled\nEthernet\n",
                    if net.wifi_disabled { "*" } else { "" }
                )),
                "-getdnsservers" => Ok(net
                    .values
                    .get(&args[1])
                    .cloned()
                    .unwrap_or_else(|| vec!["1.1.1.1".into()])
                    .join("\n")),
                "-setdnsservers" => {
                    if net.fail_set.as_deref() == Some(&args[1]) {
                        net.fail_set = None;
                        anyhow::bail!("mock nonzero exit");
                    }
                    net.values.insert(args[1].clone(), args[2..].to_vec());
                    Ok(String::new())
                }
                _ => anyhow::bail!("unexpected mock command"),
            }
        })
    }

    #[test]
    fn restart_recovers_private_journal_and_skips_disabled_services() {
        let temp = TempDir::new();
        let network = Arc::new(Mutex::new(Network::default()));
        {
            let mut manager = DNSManager::at(&temp.0, mock(network.clone())).unwrap();
            manager.set_dns(vec!["10.0.0.53"], vec![]).unwrap();
            assert_eq!(fs::metadata(&manager.state).unwrap().mode() & 0o777, 0o600);
            assert!(DNSManager::at(&temp.0, mock(network.clone())).is_err());
            assert!(!network
                .lock()
                .unwrap()
                .calls
                .iter()
                .flatten()
                .any(|arg| arg.contains("Disabled")));
            // Drop without cleanup models a terminated process, not a real DNS change.
        }
        let mut restarted = DNSManager::at(&temp.0, mock(network.clone())).unwrap();
        restarted.recover_dns().unwrap();
        assert_eq!(network.lock().unwrap().values["Wi-Fi"], ["1.1.1.1"]);
        assert!(!restarted.state.exists());
    }

    #[test]
    fn restore_preserves_external_edits_and_partial_failure_rolls_back() {
        let temp = TempDir::new();
        let network = Arc::new(Mutex::new(Network::default()));
        let mut manager = DNSManager::at(&temp.0, mock(network.clone())).unwrap();
        manager.set_dns(vec!["10.0.0.53"], vec![]).unwrap();
        network
            .lock()
            .unwrap()
            .values
            .insert("Wi-Fi".into(), vec!["9.9.9.9".into()]);
        manager.restore_dns().unwrap();
        assert_eq!(network.lock().unwrap().values["Wi-Fi"], ["9.9.9.9"]);
        network.lock().unwrap().fail_set = Some("Ethernet".into());
        assert!(manager.set_dns(vec!["10.0.0.54"], vec![]).is_err());
        assert_eq!(network.lock().unwrap().values["Wi-Fi"], ["9.9.9.9"]);
        assert!(!manager.state.exists());
    }

    #[test]
    fn failed_recovery_retains_journal_and_unsafe_state_is_rejected() {
        let temp = TempDir::new();
        let network = Arc::new(Mutex::new(Network::default()));
        let mut manager = DNSManager::at(&temp.0, mock(network.clone())).unwrap();
        manager.set_dns(vec!["10.0.0.53"], vec![]).unwrap();
        network.lock().unwrap().fail_set = Some("Wi-Fi".into());
        assert!(manager.restore_dns().is_err());
        assert!(manager.state.exists());
        manager.restore_dns().unwrap();
        std::os::unix::fs::symlink(temp.0.join("absent"), &manager.state).unwrap();
        assert!(manager.restore_dns().is_err());
    }

    #[test]
    fn list_or_get_failure_does_not_write_state_or_set_dns() {
        let temp = TempDir::new();
        let mut manager =
            DNSManager::at(&temp.0, Box::new(|_| anyhow::bail!("mock nonzero exit"))).unwrap();
        assert!(manager.set_dns(vec!["10.0.0.53"], vec![]).is_err());
        assert!(!manager.state.exists());
    }
    #[test]
    fn recovery_does_not_modify_a_service_disabled_after_setup() {
        let temp = TempDir::new();
        let network = Arc::new(Mutex::new(Network::default()));
        let mut manager = DNSManager::at(&temp.0, mock(network.clone())).unwrap();
        manager.set_dns(vec!["10.0.0.53"], vec![]).unwrap();
        network.lock().unwrap().wifi_disabled = true;
        network.lock().unwrap().calls.clear();
        assert!(manager.restore_dns().is_err());
        assert!(manager.state.exists());
        {
            let net = network.lock().unwrap();
            assert!(!net
                .calls
                .iter()
                .any(|args| args.get(1).map(String::as_str) == Some("Wi-Fi")));
            assert_eq!(net.values["Wi-Fi"], ["10.0.0.53"]);
        }
        network.lock().unwrap().wifi_disabled = false;
        manager.recover_dns().unwrap();
        assert_eq!(network.lock().unwrap().values["Wi-Fi"], ["1.1.1.1"]);
        assert!(!manager.state.exists());
    }
}
