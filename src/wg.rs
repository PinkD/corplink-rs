use base64::Engine;
use std::ffi::{c_void, CStr, CString};
use std::net::{IpAddr, SocketAddr};
use std::time;

use anyhow::{anyhow, bail, ensure, Context, Result};

use crate::config;

#[allow(clippy::all)]
#[allow(
    dead_code,
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals
)]
mod libwg {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

fn start_wg(log_level: i32, protocol: i32, interface_name: &str) -> Result<i32> {
    let input_cstring =
        CString::new(interface_name.as_bytes()).context("buff contains null character")?;
    unsafe { Ok(libwg::startWg(log_level, protocol, input_cstring.as_ptr())) }
}

fn stop_wg() {
    unsafe {
        libwg::stopWg();
    }
}

fn start_wg_netstack(
    log_level: i32,
    protocol: i32,
    addresses: &str,
    dns: &str,
    socks_listen: &str,
    socks_user: &str,
    socks_pass: &str,
    mtu: i32,
) -> Result<i32> {
    let c_addresses = CString::new(addresses).context("addresses contains null character")?;
    let c_dns = CString::new(dns).context("dns contains null character")?;
    let c_socks = CString::new(socks_listen).context("socks_listen contains null character")?;
    let c_user = CString::new(socks_user).context("socks_user contains null character")?;
    let c_pass = CString::new(socks_pass).context("socks_pass contains null character")?;
    unsafe {
        Ok(libwg::startWgNetstack(
            log_level,
            protocol,
            c_addresses.as_ptr(),
            c_dns.as_ptr(),
            c_socks.as_ptr(),
            c_user.as_ptr(),
            c_pass.as_ptr(),
            mtu,
        ))
    }
}

fn uapi(buff: &[u8]) -> Result<Vec<u8>> {
    let input_cstring = CString::new(buff).context("buff contains null character")?;
    unsafe {
        let result_ptr = libwg::uapi(input_cstring.as_ptr());
        if result_ptr.is_null() {
            return Err(anyhow!("libwg::uapi() returned null pointer"));
        }
        let result = CStr::from_ptr(result_ptr).to_bytes().to_vec();
        libc::free(result_ptr as *mut c_void);
        Ok(result)
    }
}

pub fn stop_wg_go() {
    stop_wg();
}

pub fn start_wg_go(name: &str, protocol: i32, with_log: bool) -> Result<()> {
    ensure!(
        !name.is_empty() && !name.chars().any(char::is_control),
        "invalid interface name"
    );
    ensure!(matches!(protocol, 0 | 1), "unsupported WireGuard transport");
    log::info!("start wg-corplink");
    let mut log_level = libwg::LogLevelError;
    if with_log {
        log_level = libwg::LogLevelVerbose;
    }
    let ret = start_wg(log_level, protocol, name)?;
    if !matches!(ret, 0) {
        return Err(anyhow!("start_wg returned non-zero code: {ret}"));
    }
    Ok(())
}

// start wg-corplink in userspace netstack mode and expose a SOCKS5 proxy.
// no kernel TUN device, no system routes/dns and no root are needed.
pub fn start_wg_go_netstack(
    conf: &config::WgConf,
    socks_listen: &str,
    socks_user: &str,
    socks_pass: &str,
    with_log: bool,
) -> Result<()> {
    validate_socks_config(socks_listen, socks_user, socks_pass)?;
    // Validate the complete configuration before creating any interface/listener.
    build_uapi_config(conf, true)?;
    log::info!("start wg-corplink in netstack/socks5 mode");
    let log_level = if with_log {
        libwg::LogLevelVerbose
    } else {
        libwg::LogLevelError
    };
    let mut addrs = vec![conf.address.clone()];
    if !conf.address6.is_empty() {
        addrs.push(conf.address6.clone());
    }
    let addresses = addrs.join(",");
    let ret = start_wg_netstack(
        log_level,
        conf.protocol,
        &addresses,
        &conf.dns,
        socks_listen,
        socks_user,
        socks_pass,
        conf.mtu as i32,
    )?;
    if !matches!(ret, 0) {
        return Err(anyhow!("start_wg_netstack returned non-zero code: {ret}"));
    }
    Ok(())
}

// Accept literal listen addresses so a hostname cannot resolve to a public
// interface after a loopback-only authentication check.
pub fn validate_socks_config(listen: &str, user: &str, pass: &str) -> Result<()> {
    let addr: SocketAddr = listen
        .parse()
        .context("socks5_listen must be an IP address and port")?;
    ensure!(addr.port() != 0, "socks5_listen port must be nonzero");
    ensure!(
        user.is_empty() == pass.is_empty(),
        "SOCKS authentication requires both a nonempty username and password"
    );
    ensure!(
        user.len() <= 255 && pass.len() <= 255,
        "SOCKS credentials must be at most 255 bytes"
    );
    ensure!(
        !user.contains('\0') && !pass.contains('\0'),
        "SOCKS credentials contain a null byte"
    );
    // IPv4-mapped IPv6 loopback is intentionally treated as non-loopback.
    ensure!(
        addr.ip().is_loopback() || !user.is_empty(),
        "non-loopback SOCKS listeners require username/password authentication"
    );
    Ok(())
}

fn cidr(value: &str, field: &str, require_prefix: bool) -> Result<String> {
    let (address, prefix) = match value.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (value, None),
    };
    let address: IpAddr = address
        .parse()
        .with_context(|| format!("invalid {field} IP address"))?;
    let max = if address.is_ipv4() { 32 } else { 128 };
    let prefix: u8 = match prefix {
        Some(value) => {
            ensure!(
                !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()),
                "invalid {field} prefix"
            );
            value
                .parse()
                .with_context(|| format!("invalid {field} prefix"))?
        }
        None if !require_prefix => max,
        None => bail!("missing {field} prefix"),
    };
    ensure!(prefix <= max, "invalid {field} prefix length");
    Ok(format!("{address}/{prefix}"))
}

fn endpoint(value: &str) -> Result<String> {
    if let Ok(addr) = value.parse::<SocketAddr>() {
        ensure!(addr.port() != 0, "endpoint port must be nonzero");
        return Ok(addr.to_string());
    }
    // Preserve DNS endpoint support without passing unchecked protocol text.
    let (host, port) = value
        .rsplit_once(':')
        .context("endpoint requires a host and port")?;
    ensure!(
        !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()),
        "invalid endpoint port"
    );
    ensure!(
        port.parse::<u16>().context("invalid endpoint port")? != 0,
        "endpoint port must be nonzero"
    );
    let domain = host.strip_suffix('.').unwrap_or(host);
    ensure!(
        !domain.is_empty()
            && domain.len() <= 253
            && domain.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }),
        "invalid endpoint hostname"
    );
    Ok(format!("{host}:{port}"))
}

fn key_hex(value: &str, field: &str) -> Result<String> {
    let key = base64::engine::general_purpose::STANDARD
        .decode(value)
        .with_context(|| format!("invalid {field} base64"))?;
    ensure!(key.len() == 32, "{field} must contain exactly 32 bytes");
    ensure!(key.iter().any(|b| *b != 0), "{field} cannot be zero");
    Ok(hex::encode(key))
}

// This boundary is independent of response parsing: never serialize an
// unchecked string into the newline-delimited, privileged UAPI protocol.
pub(crate) fn build_uapi_config(conf: &config::WgConf, netstack: bool) -> Result<String> {
    let private_key = key_hex(&conf.private_key, "private key")?;
    let public_key = key_hex(&conf.peer_key, "peer key")?;
    let endpoint = endpoint(&conf.peer_address)?;
    let address = cidr(&conf.address, "interface address", true)?;
    ensure!(
        conf.address
            .split('/')
            .next()
            .unwrap_or("")
            .parse::<IpAddr>()?
            .is_ipv4(),
        "interface address must be IPv4"
    );
    let address6 = if conf.address6.is_empty() {
        None
    } else {
        let parsed = cidr(&conf.address6, "IPv6 interface address", true)?;
        ensure!(
            conf.address6
                .split('/')
                .next()
                .unwrap_or("")
                .parse::<IpAddr>()?
                .is_ipv6(),
            "IPv6 interface address must be IPv6"
        );
        Some(parsed)
    };
    ensure!((576..=65535).contains(&conf.mtu), "invalid interface MTU");
    ensure!(
        matches!(conf.protocol, 0 | 1),
        "unsupported WireGuard transport"
    );
    if !conf.dns.is_empty() {
        for server in conf.dns.split(',') {
            server
                .parse::<IpAddr>()
                .context("invalid VPN DNS address")?;
        }
    }
    let allowed: Vec<_> = conf
        .allowed_ips
        .iter()
        .map(|v| cidr(v, "allowed IP", false))
        .collect::<Result<_>>()?;
    let routes: Vec<_> = conf
        .routes
        .iter()
        .map(|v| cidr(v, "route", false))
        .collect::<Result<_>>()?;
    let mut buff = format!("set=1\nprivate_key={private_key}\nreplace_peers=true\npublic_key={public_key}\nreplace_allowed_ips=true\nendpoint={endpoint}\npersistent_keepalive_interval=10\n");
    for route in allowed {
        buff.push_str(&format!("allowed_ip={route}\n"));
    }
    if !netstack {
        buff.push_str(&format!("address={address}\n"));
        if let Some(address6) = address6 {
            buff.push_str(&format!("address={address6}\n"));
        }
        buff.push_str(&format!("mtu={}\nup=true\n", conf.mtu));
        for route in routes {
            buff.push_str(&format!("route={route}\n"));
        }
    }
    buff.push('\n');
    Ok(buff)
}

fn apply_uapi(buff: &str) -> Result<()> {
    let data = uapi(buff.as_bytes()).context("call uapi")?;
    let response = std::str::from_utf8(&data).context("failed to decode uapi response")?;
    ensure!(
        response.trim_end_matches('\n') == "errno=0",
        "UAPI configuration failed"
    );
    Ok(())
}

pub struct UAPIClient {
    pub name: String,
}

impl UAPIClient {
    pub async fn config_wg(&mut self, conf: &config::WgConf) -> Result<()> {
        apply_uapi(&build_uapi_config(conf, false)?)
    }

    pub async fn config_wg_netstack(&mut self, conf: &config::WgConf) -> Result<()> {
        apply_uapi(&build_uapi_config(conf, true)?)
    }

    pub async fn check_wg_connection(&mut self) {
        // default refresh key timeout of wg is 2 min
        // we set wg connection timeout to 5 min
        let interval = time::Duration::from_secs(5 * 60);
        let mut ticker = tokio::time::interval(interval);
        let mut timeout = false;
        // consume the first tick
        ticker.tick().await;
        while !timeout {
            ticker.tick().await;

            let name = self.name.as_str();
            let data = match uapi(b"get=1\n\n") {
                Ok(data) => data,
                Err(e) => {
                    log::warn!("failed to call uapi for {}: {}", name, e);
                    continue;
                }
            };
            let s = match String::from_utf8(data) {
                Ok(s) => s,
                Err(err) => {
                    log::warn!("failed to parse uapi response for {}: {}", name, err);
                    continue;
                }
            };
            for line in s.split('\n') {
                if line.starts_with("last_handshake_time_sec") {
                    let last = match line.trim_end().split('=').next_back() {
                        Some(v) => v,
                        None => {
                            log::warn!("unexpected uapi line: {}", line);
                            continue;
                        }
                    };
                    match last.parse::<i64>() {
                        Ok(timestamp) => {
                            if timestamp == 0 {
                                // do nothing because it's invalid
                            } else if let Some(nt) = chrono::DateTime::from_timestamp(timestamp, 0)
                            {
                                let now = chrono::Utc::now().to_utc();
                                let t = now - nt;
                                let tt = nt.to_utc();
                                let lt = tt.with_timezone(&chrono::Local);
                                if let Ok(elapsed) = t.to_std() {
                                    let elapsed = elapsed.as_secs_f32();
                                    log::info!(
                                        "last handshake is at {lt}, elapsed time {elapsed}s"
                                    );
                                    if let Ok(interval_dur) = chrono::Duration::from_std(interval) {
                                        if t > interval_dur {
                                            log::warn!(
                                                    "last handshake is at {}, elapsed time {}s more than {}s",
                                                    lt,
                                                    elapsed,
                                                    interval.as_secs()
                                                );
                                            timeout = true;
                                        }
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            log::warn!("parse last handshake of {} fail: {}", name, err)
                        }
                    }
                    break;
                } else if line.starts_with("errno") {
                    if line != "errno=0" {
                        log::warn!("uapi of {} return: fail: {}", name, line)
                    }
                } else if line.is_empty() {
                    // reach end
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> config::WgConf {
        config::WgConf {
            address: "10.1.2.3/24".into(),
            address6: "fd00::2/128".into(),
            peer_address: "192.0.2.1:51820".into(),
            mtu: 1420,
            public_key: String::new(),
            private_key: base64::engine::general_purpose::STANDARD.encode([1; 32]),
            peer_key: base64::engine::general_purpose::STANDARD.encode([2; 32]),
            allowed_ips: vec!["10.0.0.0/8".into(), "fd00::3".into()],
            routes: Vec::new(),
            dns: "10.0.0.1".into(),
            protocol: 0,
        }
    }

    #[test]
    fn config_preserves_route_policy_and_ipv6_host_prefix() {
        let conf = config();
        let tun = build_uapi_config(&conf, false).unwrap();
        assert!(tun.contains("allowed_ip=fd00::3/128\n"));
        assert!(!tun.lines().any(|line| line.starts_with("route=")));
        let proxy = build_uapi_config(&conf, true).unwrap();
        for operation in ["address=", "mtu=", "up=", "route="] {
            assert!(!proxy.lines().any(|line| line.starts_with(operation)));
        }
    }

    #[test]
    fn every_remote_text_boundary_rejects_protocol_injection() {
        for value in [
            "10.1.2.3/24\nup=true\nroute=0.0.0.0/0",
            "10.1.2.3/24\r",
            "10.1.2.3/24\0",
        ] {
            let mut conf = config();
            conf.address = value.into();
            assert!(build_uapi_config(&conf, false).is_err());
        }
        for field in 0..6 {
            let mut conf = config();
            match field {
                0 => conf.address6 = "fd00::1/128\nroute=::/0".into(),
                1 => conf.peer_address = "vpn.example:51820\nup=true".into(),
                2 => conf.allowed_ips.push("10.0.0.0/8\nroute=0.0.0.0/0".into()),
                3 => conf.routes.push("10.0.0.0/8\nup=true".into()),
                4 => conf.private_key.push('\n'),
                _ => conf.dns = "10.0.0.1\nsearch attacker.example".into(),
            }
            assert!(build_uapi_config(&conf, false).is_err());
            assert!(build_uapi_config(&conf, true).is_err());
        }
    }

    #[test]
    fn invalid_keys_addresses_and_endpoints_fail_before_ffi() {
        let mut conf = config();
        conf.private_key = base64::engine::general_purpose::STANDARD.encode([1; 31]);
        assert!(build_uapi_config(&conf, false).is_err());
        assert!(cidr("10.0.0.1/33", "test", true).is_err());
        assert!(cidr("::1/129", "test", true).is_err());
        assert!(endpoint("vpn.example:51820").is_ok());
        assert!(endpoint("[2001:db8::1]:51820").is_ok());
        for invalid in [
            "vpn.example:0",
            "vpn.example:+80",
            "vpn.example:65536",
            "bad host:80",
            "-option:80",
        ] {
            assert!(endpoint(invalid).is_err());
        }
    }

    #[test]
    fn socks_requires_complete_credentials_for_remote_listeners() {
        assert!(validate_socks_config("127.0.0.1:1080", "", "").is_ok());
        assert!(validate_socks_config("[::1]:1080", "", "").is_ok());
        assert!(validate_socks_config("0.0.0.0:1080", "user", "pass").is_ok());
        for (listen, user, pass) in [
            ("0.0.0.0:1080", "", ""),
            ("[::]:1080", "", ""),
            ("127.0.0.1:1080", "", "pass"),
            ("127.0.0.1:1080", "user", ""),
            ("localhost:1080", "", ""),
        ] {
            assert!(validate_socks_config(listen, user, pass).is_err());
        }
    }
}
