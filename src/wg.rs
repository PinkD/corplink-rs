use std::io::{Error, ErrorKind};
use std::path::Path;
use std::process::Stdio;
use std::time;

use chrono;
use tokio::process::Command;

use crate::template::Template;
use crate::{config, utils};

const CMD_SYSTEMCTL: &str = "systemctl";
const CMD_WG: &str = "wg";

async fn cmd_exist(cmd: &str) -> bool {
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

pub async fn gen_wg_conf(file: impl AsRef<Path>, conf: &config::WgConf) -> Result<(), Error> {
    let tmpl: Template = Template::new(config::WG_CONF_TEMPLATE);
    let conf = tmpl.render((*conf).clone());
    tokio::fs::write(file, conf).await
}

pub async fn start_wg_quick(name: &String) {
    let service = utils::service_name(name);
    if !cmd_exist(CMD_SYSTEMCTL).await {
        println!(
            "skip start {} becuase {} not found, you may need to start wireguard by your self",
            service, CMD_SYSTEMCTL
        )
    }
    let mut process = Command::new(CMD_SYSTEMCTL)
        .args(["start", service.as_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    match process.wait().await {
        Ok(code) => {
            if code.success() {
                println!("{} started", service)
            } else {
                println!("start {} fail: {}", service, code)
            }
        }
        Err(err) => {
            println!("start {} fail: {}", service, err)
        }
    }
}

pub async fn stop_wg_quick(name: &String) {
    if !cmd_exist(CMD_SYSTEMCTL).await {
        return;
    }
    let service = utils::service_name(name);
    let mut process = tokio::process::Command::new("systemctl")
        .args(["stop", service.as_str()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    match process.wait().await {
        Ok(code) => {
            if code.success() {
                println!("{} stopped", service)
            } else {
                println!("stop {} fail: {}", service, code)
            }
        }
        Err(err) => {
            println!("stop {} fail: {}", service, err)
        }
    }
}

pub async fn check_wg_connection(name: &String) {
    // default refresh key timeout of wg is 2 min
    // we set wg connection timeout to 5 min
    let interval = time::Duration::from_secs(5 * 60);
    let mut ticker = tokio::time::interval(interval);
    let mut timeout = false;
    if !cmd_exist(CMD_WG).await {
        println!(
            "skip check handshake timeout becuase command {} not found",
            CMD_WG
        );
        loop {
            // tick forever because we cannot check handshake
            ticker.tick().await;
        }
    }
    // consume the first tick
    ticker.tick().await;
    while !timeout {
        ticker.tick().await;
        match tokio::process::Command::new(CMD_WG)
            .args(["show", name.as_str(), "latest-handshakes"])
            .output()
            .await
        {
            Ok(output) => {
                let byte_to_str =
                    |v: Vec<u8>| String::from(std::str::from_utf8(v.as_slice()).unwrap());
                if output.status.success() {
                    let stdout = byte_to_str(output.stdout);

                    let s = stdout.split_whitespace();
                    match s.last().unwrap().parse::<i64>() {
                        Ok(timestamp) => {
                            let nt = chrono::NaiveDateTime::from_timestamp(timestamp, 0);
                            let now = chrono::Utc::now().naive_utc();
                            let t = now - nt;
                            let tt: chrono::DateTime<chrono::Utc> =
                                chrono::DateTime::from_utc(nt, chrono::Utc);
                            let lt = tt.with_timezone(&chrono::Local);
                            let elapsed = t.to_std().unwrap().as_secs_f32();
                            println!("last handshake is at {}, elapsed time {}s", lt, elapsed);
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
                        Err(err) => {
                            println!("parse last handshake of {} fail: {}", name, err)
                        }
                    }
                } else {
                    println!(
                        "get last handshake of {} fail, stdout: {}, stderr: {}",
                        name,
                        byte_to_str(output.stdout),
                        byte_to_str(output.stderr)
                    )
                }
            }
            Err(err) => {
                println!("get last handshake of {} fail: {}", name, err)
            }
        };
    }
}
