use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::process::Stdio;
use std::time;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(windows)]
use tokio::net::windows::named_pipe::ClientOptions;
use tokio::process::Command;

use crate::{config, utils};

pub const ENV_KEY_PROTOCOL_VERSION: &str = "CORPLINK_PROTOCOL_VERSION";
pub const ENV_KEY_NETWORK_TYPE: &str = "CORPLINK_NETWORK_TYPE";

pub async fn cmd_exist(cmd: &str) -> bool {
    match Command::new(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut p) => match p.wait().await {
            Ok(status) => status.success(),
            Err(e) => {
                println!("{} exists but cannot execute correctly: {}", cmd, e);
                false
            }
        },
        Err(e) => {
            if let ErrorKind::NotFound = e.kind() {
                false
            } else {
                println!("failed to check {} exist: {}", cmd, e);
                false
            }
        }
    }
}

pub async fn start_wg_go(
    cmd: &str,
    name: &str,
    protocol: i32,
    protocol_version: &str,
) -> io::Result<tokio::process::Child> {
    let mut envs = HashMap::new();

    if protocol_version == "v2" {
        envs.insert(ENV_KEY_PROTOCOL_VERSION, "v2");
    }

    // TODO: replace with real protocol and support tcp tun
    if protocol == 0xff {
        envs.insert(ENV_KEY_NETWORK_TYPE, "tcp");
    }

    println!("launch {cmd} with env: {envs:?}");
    return Command::new(cmd)
        .args(["-f", name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .envs(envs)
        .spawn();
}

const SOCKET_DIRECTORY_UNIX: &str = "/var/run/wireguard";
#[cfg(windows)]
const SOCKET_DIRECTORY_WINDOWS: &str = r"\\.\pipe\ProtectedPrefix\Administrators\WireGuard\";

cfg_if::cfg_if! {
    if #[cfg(windows)] {
        type Conn = tokio::net::windows::named_pipe::NamedPipeClient;
    } else {
        type Conn = tokio::net::UnixStream;
    }
}
pub struct UAPIClient {
    pub name: String,
}

impl UAPIClient {
    pub async fn connect_uapi(&mut self) -> io::Result<Conn> {
        cfg_if::cfg_if! {
            if #[cfg(windows)] {
                let path = format!("{}\\{}", SOCKET_DIRECTORY_WINDOWS, self.name);
                wait_path_exist(&path).await;
                let conn = ClientOptions::new().open(path)?;
                // TODO: wait socket ready
            } else {
                let path = format!("{}/{}.sock", SOCKET_DIRECTORY_UNIX, self.name);
                wait_path_exist(&path).await;
                let conn = tokio::net::UnixStream::connect(path).await?;
                loop {
                    let ready = conn.ready(tokio::io::Interest::WRITABLE).await?;
                    if ready.is_writable() {
                        break;
                    } else {
                        println!("uapi not ready, sleep 1s");
                        tokio::time::sleep(time::Duration::from_secs(1)).await;
                    }
                }
            }
        }
        Ok(conn)
    }

    pub async fn config_wg(&mut self, conf: &config::WgConf) -> io::Result<()> {
        let mut conn = self.connect_uapi().await?;
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
        let data = buff.as_bytes();

        println!("send config to uapi");
        match conn.write(data).await {
            Ok(_) => {}
            Err(err) => {
                return Err(err);
            }
        }
        conn.flush().await?;
        let mut s = String::new();
        let mut reader = BufReader::new(conn);
        match reader.read_line(&mut s).await {
            Ok(_) => {}
            Err(err) => {
                return Err(err);
            }
        }
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

            let mut conn = match self.connect_uapi().await {
                Ok(conn) => conn,
                Err(err) => {
                    println!("failed to connect uapi: {}", err);
                    return;
                }
            };
            let name = self.name.as_str();
            match conn.write(b"get=1\n\n").await {
                Ok(_) => {}
                Err(err) => {
                    println!("get last handshake of {} fail: {}", name, err)
                }
            }
            let mut line = String::new();
            let mut reader = BufReader::new(conn);
            loop {
                match reader.read_line(&mut line).await {
                    Ok(_) => {
                        if line.starts_with("last_handshake_time_sec") {
                            match line.trim_end().split('=').last().unwrap().parse::<i64>() {
                                Ok(timestamp) => {
                                    if timestamp == 0 {
                                        // do nothing because it's invalid
                                    } else {
                                        let nt =
                                            chrono::NaiveDateTime::from_timestamp(timestamp, 0);
                                        let now = chrono::Utc::now().naive_utc();
                                        let t = now - nt;
                                        let tt: chrono::DateTime<chrono::Utc> =
                                            chrono::DateTime::from_utc(nt, chrono::Utc);
                                        let lt = tt.with_timezone(&chrono::Local);
                                        let elapsed = t.to_std().unwrap().as_secs_f32();
                                        println!(
                                            "last handshake is at {lt}, elapsed time {elapsed}s"
                                        );
                                        if t > chrono::Duration::from_std(interval).unwrap() {
                                            println!(
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
                                    println!("parse last handshake of {} fail: {}", name, err)
                                }
                            }
                            break;
                        } else if line == "\n" {
                            // reach end
                            break;
                        }
                        // clear line cache for next read
                        line.clear();
                    }
                    Err(err) => {
                        println!("get last handshake of {} fail: {}", name, err);
                        break;
                    }
                }
            }
        }
    }
}

async fn wait_path_exist(file: &str) {
    let mut count = 10;
    while count > 0 {
        if std::path::Path::new(file).exists() {
            return;
        }
        println!("socket file {file} not ready, sleep 1s");
        tokio::time::sleep(time::Duration::from_secs(1)).await;
        count -= 1;
    }
}
