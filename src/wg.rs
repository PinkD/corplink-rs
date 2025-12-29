use std::ffi::{c_void, CStr, CString};
use std::time;

use anyhow::{anyhow, Context, Result};

use crate::{config, utils};

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

pub struct UAPIClient {
    pub name: String,
}

impl UAPIClient {
    pub async fn config_wg(&mut self, conf: &config::WgConf) -> Result<()> {
        let mut buff = String::from("set=1\n");
        // standard wg-go uapi operations
        // see https://www.wireguard.com/xplatform/#configuration-protocol
        let private_key = utils::b64_decode_to_hex(&conf.private_key)?;
        let public_key = utils::b64_decode_to_hex(&conf.peer_key)?;
        buff.push_str(format!("private_key={private_key}\n").as_str());
        buff.push_str("replace_peers=true\n".to_string().as_str());
        buff.push_str(format!("public_key={public_key}\n").as_str());
        buff.push_str("replace_allowed_ips=true\n".to_string().as_str());
        buff.push_str(format!("endpoint={}\n", conf.peer_address).as_str());
        buff.push_str("persistent_keepalive_interval=10\n".to_string().as_str());
        for route in &conf.route {
            if route.contains("/") {
                buff.push_str(format!("allowed_ip={route}\n").as_str());
            } else {
                buff.push_str(format!("allowed_ip={route}/32\n").as_str());
            }
        }

        // wg-corplink uapi operations
        let addr = &conf.address;
        let addr6 = &conf.address6;
        let mtu = conf.mtu;
        buff.push_str(format!("address={addr}\n").as_str());
        if !addr6.is_empty() {
            buff.push_str(format!("address={addr6}\n").as_str());
        }
        buff.push_str(format!("mtu={mtu}\n").as_str());
        buff.push_str("up=true\n".to_string().as_str());
        for route in &conf.route {
            if route.contains("/") {
                buff.push_str(format!("route={route}\n").as_str());
            } else {
                let prefix_len = if route.contains(":") { 128 } else { 32 };
                buff.push_str(format!("route={route}/{prefix_len}\n").as_str());
            }
        }
        // end operation

        buff.push('\n');
        log::info!("send config to uapi");
        let data = uapi(buff.as_bytes()).context("call uapi")?;
        let s = String::from_utf8(data).context("failed to decode uapi response")?;
        if !s.contains("errno=0") {
            return Err(anyhow!("uapi returns unexpected result: {}", s));
        }
        Ok(())
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
