use std::collections::HashMap;
use std::process::Command;

use anyhow::{Context, Result};

pub struct DNSManager {
    service_dns: HashMap<String, String>,
    service_dns_search: HashMap<String, String>,
}

impl DNSManager {
    pub fn new() -> DNSManager {
        DNSManager {
            service_dns: HashMap::new(),
            service_dns_search: HashMap::new(),
        }
    }

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
