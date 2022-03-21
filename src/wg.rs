use std::io::{Error, ErrorKind};
use std::path::Path;
use std::process::Stdio;

use tokio::process::Command;

use crate::template::Template;
use crate::{config, utils};

const CMD: &str = "systemctl";

async fn systemd_exist() -> bool {
    match Command::new(CMD)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut p) => match p.wait().await {
            Ok(_) => true,
            Err(e) =>  {
                println!("{} exists but cannot execute correctly: {}", CMD, e);
                false
            },
        },
        Err(e) => {
            if let ErrorKind::NotFound = e.kind() {
                false
            } else {
                println!("failed to check {} exist: {}", CMD, e);
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
    if !systemd_exist().await {
        println!("skip start {} becuase systemctl not found, you may need to start wireguard by your self", service)
    }
    let mut process = Command::new(CMD)
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
    if !systemd_exist().await {
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
