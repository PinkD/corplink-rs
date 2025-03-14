use std::io::Error;
use std::process::Command;
use super::DNSManagerTrait;

pub struct DNSManager {
    interface: String,
    original_dns: Option<String>,
    original_search: Option<String>,
}

impl DNSManagerTrait for DNSManager {
    fn new() -> DNSManager {
        DNSManager {
            interface: String::new(),
            original_dns: None,
            original_search: None,
        }
    }

    fn set_dns(&mut self, dns_servers: Vec<&str>, dns_search: Vec<&str>) -> Result<(), Error> {
        if dns_servers.is_empty() || self.interface.is_empty() {
            return Ok(());
        }

        // Store current DNS settings before changing them
        let status = Command::new("resolvectl")
            .arg("status")
            .arg(&self.interface)
            .output()?;
        let output = String::from_utf8_lossy(&status.stdout);
        
        // Parse and store original DNS servers and search domains
        for line in output.lines() {
            if line.contains("DNS Servers:") {
                self.original_dns = Some(line.split(':').nth(1).unwrap_or("").trim().to_string());
            } else if line.contains("DNS Domain:") {
                self.original_search = Some(line.split(':').nth(1).unwrap_or("").trim().to_string());
            }
        }

        // Set new DNS servers
        Command::new("resolvectl")
            .arg("dns")
            .arg(&self.interface)
            .args(dns_servers.clone())
            .status()?;

        // Set new search domains if provided
        if !dns_search.is_empty() {
            Command::new("resolvectl")
                .arg("domain")
                .arg(&self.interface)
                .args(dns_search)
                .status()?;
        }

        log::debug!(
            "DNS set for interface {} with servers: {}",
            self.interface,
            dns_servers.join(",")
        );

        Ok(())
    }

    fn restore_dns(&self) -> Result<(), Error> {
        if self.interface.is_empty() {
            return Ok(());
        }

        // Restore original DNS servers if they were saved
        if let Some(dns) = &self.original_dns {
            if !dns.is_empty() {
                Command::new("resolvectl")
                    .arg("dns")
                    .arg(&self.interface)
                    .args(dns.split_whitespace())
                    .status()?;
            }
        }

        // Restore original search domains if they were saved
        if let Some(search) = &self.original_search {
            if !search.is_empty() {
                Command::new("resolvectl")
                    .arg("domain")
                    .arg(&self.interface)
                    .args(search.split_whitespace())
                    .status()?;
            }
        }

        log::debug!("DNS settings restored for interface {}", self.interface);
        Ok(())
    }
}

impl DNSManager {
    pub fn with_interface(interface: String) -> Self {
        DNSManager {
            interface,
            original_dns: None,
            original_search: None,
        }
    }
} 