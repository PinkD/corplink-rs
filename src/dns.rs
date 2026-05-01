use anyhow::{Context, Result};

#[cfg(target_os = "macos")]
use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::process::Command;

#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};

#[cfg(target_os = "linux")]
const RESOLV_CONF_PATH: &str = "/etc/resolv.conf";

#[cfg(target_os = "linux")]
const LINUX_DEFAULT_BACKUP_FILENAME: &str = "resolv.conf.corplink";

pub struct DNSManager {
    #[cfg(target_os = "macos")]
    service_dns: HashMap<String, String>,
    #[cfg(target_os = "macos")]
    service_dns_search: HashMap<String, String>,

    #[cfg(target_os = "linux")]
    backup_path: PathBuf,
}

impl DNSManager {
    pub fn new(_backup_filename: Option<String>) -> DNSManager {
        DNSManager {
            #[cfg(target_os = "macos")]
            service_dns: HashMap::new(),
            #[cfg(target_os = "macos")]
            service_dns_search: HashMap::new(),

            #[cfg(target_os = "linux")]
            backup_path: {
                let filename = _backup_filename
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| LINUX_DEFAULT_BACKUP_FILENAME.to_string());
                Path::new(RESOLV_CONF_PATH)
                    .parent()
                    .unwrap_or_else(|| Path::new("/etc"))
                    .join(filename)
            },
        }
    }

    #[cfg(target_os = "linux")]
    #[allow(dead_code)]
    pub fn backup_path(&self) -> &Path {
        &self.backup_path
    }
}

#[cfg(target_os = "macos")]
impl DNSManager {
    fn collect_new_service_dns(&mut self) -> Result<()> {
        let output = Command::new("networksetup")
            .arg("-listallnetworkservices")
            .output()
            .context("failed to list network services")?;

        let services = String::from_utf8_lossy(&output.stdout);
        let lines = services.lines();
        // Skip the first line's legend
        for service in lines.skip(1) {
            // Remove leading '*' and trim whitespace
            let service = service.trim_start_matches('*').trim();
            if service.is_empty() {
                continue;
            }

            // get DNS servers
            let dns_output = Command::new("networksetup")
                .arg("-getdnsservers")
                .arg(service)
                .output()
                .with_context(|| format!("failed to get dns servers for {service}"))?;
            let dns_response = String::from_utf8_lossy(&dns_output.stdout)
                .trim()
                .to_string();
            // if dns config for this service is not empty, output should be ip addresses seperated in lines without space
            // otherwise, output should be "There aren't any DNS Servers set on xxx", use "Empty" instead, which can be recognized in 'networksetup -setdnsservers'
            let dns_response = if dns_response.contains(" ") {
                "Empty".to_string()
            } else {
                dns_response
            };

            self.service_dns
                .insert(service.to_string(), dns_response.clone());

            // get search domain
            let search_output = Command::new("networksetup")
                .arg("-getsearchdomains")
                .arg(service)
                .output()
                .with_context(|| format!("failed to get search domains for {service}"))?;
            let search_response = String::from_utf8_lossy(&search_output.stdout)
                .trim()
                .to_string();
            let search_response = if search_response.contains(" ") {
                "Empty".to_string()
            } else {
                search_response
            };

            self.service_dns_search
                .insert(service.to_string(), search_response.clone());

            log::debug!(
                "DNS collected for {}, dns servers: {}, search domain: {}",
                service,
                dns_response,
                search_response
            )
        }
        Ok(())
    }

    pub fn set_dns(&mut self, dns_servers: Vec<&str>, dns_search: Vec<&str>) -> Result<()> {
        if dns_servers.is_empty() {
            return Ok(());
        }
        self.collect_new_service_dns()?;
        for service in self.service_dns.keys() {
            Command::new("networksetup")
                .arg("-setdnsservers")
                .arg(service)
                .args(&dns_servers)
                .status()
                .with_context(|| format!("failed to set dns servers for {service}"))?;

            if !dns_search.is_empty() {
                Command::new("networksetup")
                    .arg("-setsearchdomains")
                    .arg(service)
                    .args(&dns_search)
                    .status()
                    .with_context(|| format!("failed to set search domains for {service}"))?;
            }
            log::debug!("DNS set for {} with {}", service, dns_servers.join(","));
        }

        Ok(())
    }

    pub fn restore_dns(&self) -> Result<()> {
        for (service, dns) in &self.service_dns {
            Command::new("networksetup")
                .arg("-setdnsservers")
                .arg(service)
                .args(dns.lines())
                .status()
                .with_context(|| format!("failed to reset dns servers for {service}"))?;

            log::debug!("DNS server reset for {} with {}", service, dns);
        }
        for (service, search_domain) in &self.service_dns_search {
            Command::new("networksetup")
                .arg("-setsearchdomains")
                .arg(service)
                .args(search_domain.lines())
                .status()
                .with_context(|| format!("failed to reset search domains for {service}"))?;
            log::debug!(
                "DNS search domain reset for {} with {}",
                service,
                search_domain
            )
        }
        log::debug!("DNS reset");
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl DNSManager {
    pub fn set_dns(&mut self, dns_servers: Vec<&str>, dns_search: Vec<&str>) -> Result<()> {
        if dns_servers.is_empty() {
            return Ok(());
        }

        if self.backup_path.exists() {
            log::warn!(
                "existing backup at {} — a previous instance likely did not exit \
                 gracefully; keeping that file as the authoritative pre-override",
                self.backup_path.display()
            );
        } else {
            if let Err(e) = fs::rename(RESOLV_CONF_PATH, &self.backup_path) {
                log::warn!(
                    "could not back up {} to {}: {e}. \
                     Overriding without backup; restore on exit will be a no-op.",
                    RESOLV_CONF_PATH,
                    self.backup_path.display()
                );
            } else {
                log::info!(
                    "renamed {} -> {} for backup",
                    RESOLV_CONF_PATH,
                    self.backup_path.display()
                );
            }
        }

        let new_content = render_resolv_conf(&dns_servers, &dns_search);
        fs::write(RESOLV_CONF_PATH, &new_content)
            .with_context(|| format!("failed to write {RESOLV_CONF_PATH}"))?;

        log::info!(
            "DNS overridden in {}; servers={:?} search={:?}",
            RESOLV_CONF_PATH,
            dns_servers,
            dns_search
        );
        Ok(())
    }

    pub fn restore_dns(&self) -> Result<()> {
        if !self.backup_path.exists() {
            return Ok(());
        }
        match fs::rename(&self.backup_path, RESOLV_CONF_PATH) {
            Ok(()) => {
                log::info!(
                    "restored {} from {} (via rename)",
                    RESOLV_CONF_PATH,
                    self.backup_path.display()
                );
                Ok(())
            }
            Err(e) => {
                log::warn!(
                    "could not restore {} by renaming {} back: {e}. \
                     Leaving backup on disk.",
                    RESOLV_CONF_PATH,
                    self.backup_path.display()
                );
                Ok(())
            }
        }
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
impl DNSManager {
    pub fn set_dns(&mut self, _dns_servers: Vec<&str>, _dns_search: Vec<&str>) -> Result<()> {
        Ok(())
    }
    pub fn restore_dns(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn render_resolv_conf(dns_servers: &[&str], dns_search: &[&str]) -> String {
    let mut out = String::new();
    out.push_str("# Generated by corplink-rs (will be restored on graceful exit)\n");
    for dns in dns_servers {
        out.push_str(&format!("nameserver {dns}\n"));
    }
    if !dns_search.is_empty() {
        out.push_str(&format!("search {}\n", dns_search.join(" ")));
    }
    out
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn default_filename_when_none_given() {
        let m = DNSManager::new(None);
        let expected = Path::new(RESOLV_CONF_PATH)
            .parent()
            .unwrap()
            .join(LINUX_DEFAULT_BACKUP_FILENAME);
        assert_eq!(m.backup_path(), expected.as_path());
    }

    #[test]
    fn default_filename_when_empty_string_given() {
        let m = DNSManager::new(Some(String::new()));
        let expected = Path::new(RESOLV_CONF_PATH)
            .parent()
            .unwrap()
            .join(LINUX_DEFAULT_BACKUP_FILENAME);
        assert_eq!(m.backup_path(), expected.as_path());
    }

    #[test]
    fn custom_filename_joined_with_resolv_conf_parent() {
        let m = DNSManager::new(Some("my.bak".to_string()));
        assert_eq!(m.backup_path(), Path::new("/etc/my.bak"));
    }

    #[test]
    fn backup_path_always_in_resolv_conf_dir() {
        // Invariant: because we only take a filename and join it with
        // RESOLV_CONF_PATH's parent, the backup is always on the same fs
        // as /etc/resolv.conf — rename(2) cannot EXDEV.
        let resolv_dir = Path::new(RESOLV_CONF_PATH).parent().unwrap();
        for filename in ["resolv.conf.corplink", "other.bak", "x"] {
            let m = DNSManager::new(Some(filename.to_string()));
            assert_eq!(m.backup_path().parent().unwrap(), resolv_dir);
        }
    }

    #[test]
    fn render_single_dns_no_search() {
        let out = render_resolv_conf(&["10.8.8.18"], &[]);
        assert!(out.contains("nameserver 10.8.8.18\n"));
        assert!(!out.contains("search "));
    }

    #[test]
    fn render_multiple_dns() {
        let out = render_resolv_conf(&["10.8.8.18", "114.114.114.114"], &[]);
        assert!(out.contains("nameserver 10.8.8.18\n"));
        assert!(out.contains("nameserver 114.114.114.114\n"));
    }

    #[test]
    fn render_with_search_domains() {
        let out = render_resolv_conf(&["10.8.8.18"], &["bytedance.net", "corp.local"]);
        assert!(out.contains("search bytedance.net corp.local\n"));
    }

    #[test]
    fn render_starts_with_comment_marker() {
        let out = render_resolv_conf(&["1.1.1.1"], &[]);
        assert!(out.starts_with("# "), "expected a comment banner, got: {out}");
    }
}
