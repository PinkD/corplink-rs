use std::io;
use std::time;
use std::ffi::{c_char, c_void, CStr, CString};

use crate::{config, utils};

#[allow(clippy::all)]
#[allow(dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals)]
mod libwg {
    include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
}

fn start_wg(log_level: i32, interface_name: &str) -> i32 {
    let name = interface_name.as_bytes();
    unsafe { libwg::startWg(log_level, to_c_char_array(name)) }
}

fn stop_wg() {
    unsafe { libwg::stopWg(); }
}

unsafe fn to_c_char_array(data: &[u8]) -> *const c_char {
    CString::from_vec_unchecked(data.to_vec()).into_raw() as *const c_char
}

fn uapi(buff: &[u8]) -> Vec<u8> {
    unsafe {
        let s = libwg::uapi(to_c_char_array(buff));
        let result = CStr::from_ptr(s).to_bytes().to_vec();
        libc::free(s as *mut c_void);
        result
    }
}


pub fn stop_wg_go() {
    stop_wg();
}

pub fn start_wg_go(
    name: &str,
    protocol: i32,
    with_log: bool,
) -> bool {
    // TODO: support tcp tun
    _ = protocol;
    log::info!("start wg-corplink");
    let mut log_level = libwg::LogLevelError;
    if with_log {
        log_level = libwg::LogLevelVerbose;
    }
    let ret = start_wg(log_level, name);
    matches!(ret, 0)
}

pub struct UAPIClient {
    pub name: String,
}

impl UAPIClient {
    pub async fn config_wg(&mut self, conf: &config::WgConf) -> io::Result<()> {
        let mut buff = String::from("set=1\n");
        // standard wg-go uapi operations
        // see https://www.wireguard.com/xplatform/#configuration-protocol
        let private_key = utils::b64_decode_to_hex(&conf.private_key);
        let public_key = utils::b64_decode_to_hex(&conf.peer_key);
        buff.push_str(format!("private_key={private_key}\n").as_str());
        buff.push_str("replace_peers=true\n".to_string().as_str());
        buff.push_str(format!("public_key={public_key}\n").as_str());
        buff.push_str("replace_allowed_ips=true\n".to_string().as_str());
        buff.push_str(format!("endpoint={}\n", conf.peer_address).as_str());
        buff.push_str("persistent_keepalive_interval=10\n".to_string().as_str());
        for route in &conf.route {
            buff.push_str(format!("allowed_ip={route}\n").as_str());
        }

        // wg-corplink uapi operations
        let addr = format!("{}/{}", conf.address, conf.mask);
        let mtu = conf.mtu;
        buff.push_str(format!("address={addr}\n").as_str());
        buff.push_str(format!("mtu={mtu}\n").as_str());
        buff.push_str("up=true\n".to_string().as_str());
        for route in &conf.route {
            buff.push_str(format!("route={route}\n").as_str());
        }
        // end operation

        buff.push('\n');
        log::info!("send config to uapi");
        let data = uapi(buff.as_bytes());
        let s = String::from_utf8(data).unwrap();
        if !s.contains("errno=0") {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!("uapi returns unexpected result: {}", s),
            ));
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
            let data = uapi(b"get=1\n\n");
            let s = String::from_utf8(data).unwrap();
            for line in s.split('\n') {
                if line.starts_with("last_handshake_time_sec") {
                    match line.trim_end().split('=').last().unwrap().parse::<i64>() {
                        Ok(timestamp) => {
                            if timestamp == 0 {
                                // do nothing because it's invalid
                            } else {
                                let nt =
                                    chrono::NaiveDateTime::from_timestamp_opt(timestamp, 0)
                                        .unwrap();
                                let now = chrono::Utc::now().naive_utc();
                                let t = now - nt;
                                let tt: chrono::DateTime<chrono::Utc> =
                                    chrono::DateTime::from_utc(nt, chrono::Utc);
                                let lt = tt.with_timezone(&chrono::Local);
                                let elapsed = t.to_std().unwrap().as_secs_f32();
                                log::info!(
                                    "last handshake is at {lt}, elapsed time {elapsed}s"
                                );
                                if t > chrono::Duration::from_std(interval).unwrap() {
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

