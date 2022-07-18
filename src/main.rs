mod api;
mod client;
mod config;
mod resp;
mod state;
mod template;
mod totp;
mod utils;
mod wg;

#[cfg(windows)]
use is_elevated;
use std::{env, process::exit};

use client::Client;
use config::{Config, WgConf};

fn print_usage_and_exit(name: &str, conf: &str) {
    println!("usage:\n\t{} {}", name, conf);
    exit(1);
}

fn parse_arg() -> String {
    let mut conf_file = String::from("config.json");
    let mut args = env::args();
    // pop name
    let name = args.next().unwrap();
    match args.len() {
        0 => {}
        1 => {
            // pop arg
            let arg = args.next().unwrap();
            match arg.as_str() {
                "-h" | "--help" => {
                    print_usage_and_exit(&name, &conf_file);
                }
                _ => {
                    conf_file = arg;
                }
            }
        }
        _ => {
            print_usage_and_exit(&name, &conf_file);
        }
    }
    conf_file
}

pub const EPERM: i32 = 1;
pub const ENOENT: i32 = 2;
pub const ETIMEDOUT: i32 = 110;

#[tokio::main]
async fn main() {
    print_version();

    check_previlige();

    let conf_file = parse_arg();
    let mut conf = Config::from_file(&conf_file).await;

    let cmd = match conf.wg_binary.clone() {
        Some(cmd) => cmd,
        None => config::DEFAULT_CMD_WG_NAME.to_string(),
    };
    if !wg::cmd_exist(cmd.as_str()).await {
        println!("please download {} from the repo release page", cmd);
        exit(ENOENT)
    }

    let name = conf.interface_name.clone().unwrap();
    match conf.server {
        Some(_) => {}
        None => match client::get_company_url(conf.company_name.as_str()).await {
            Ok(resp) => {
                println!(
                    "company name is {}(zh)/{}(en) server is {}",
                    resp.zh_name, resp.en_name, resp.domain
                );
                conf.server = Some(resp.domain);
                conf.save().await;
            }
            Err(err) => {
                println!(
                    "failed to fetch company server from company name {}: {}",
                    conf.company_name, err
                );
                exit(EPERM);
            }
        },
    }

    let mut c = Client::new(conf).unwrap();
    let mut logout_retry = true;
    let wg_conf: Option<WgConf>;

    loop {
        if c.need_login() {
            println!("not login yet, try to login");
            c.login().await.unwrap();
            println!("login success");
        }
        println!("try to connect");
        match c.connect_vpn().await {
            Ok(conf) => {
                wg_conf = Some(conf);
                break;
            }
            Err(e) => {
                if logout_retry && e.to_string().contains("logout") {
                    // e contains detail message, so just print it out
                    println!("{}", e);
                    logout_retry = false;
                    continue;
                } else {
                    panic!("{}", e);
                }
            }
        };
    }
    println!("start {} for {}", cmd, &name);
    let wg_conf = wg_conf.unwrap();
    let protocol_version = wg_conf.protocol_version.clone();
    let protocol = wg_conf.protocol;
    let mut process = match wg::start_wg_go(&cmd, &name, protocol, &protocol_version).await {
        Ok(p) => p,
        Err(err) => {
            println!("failed to start wg: {}", err);
            exit(EPERM);
        }
    };
    let mut uapi = wg::UAPIClient { name: name.clone() };
    match uapi.config_wg(&wg_conf).await {
        Ok(_) => {}
        Err(err) => {
            println!("failed to config interface with uapi for {}: {}", name, err);
            exit(EPERM);
        }
    }

    let mut exit_code = 0;
    tokio::select! {
        // handle signal
        _ = async {
            match tokio::signal::ctrl_c().await {
                Ok(_) => {},
                Err(e) => {
                    println!("failed to receive signal: {}",e);
                },
            }
            println!("ctrl+v received");
        } => {},

        // keep alive
        _ = c.keep_alive_vpn(&wg_conf, 60) => {
            exit_code = ETIMEDOUT;
        },

        // check wg handshake and exit if timeout
        _ = async {
            uapi.check_wg_connection().await;
            println!("last handshake timeout");
        } => {
            exit_code = ETIMEDOUT;
        },
    }

    // shutdown
    println!("disconnecting vpn...");
    match c.disconnect_vpn(&wg_conf).await {
        Ok(_) => {}
        Err(e) => println!("failed to disconnect vpn: {}", e),
    };

    println!("killing {}...", cmd);
    match process.kill().await {
        Ok(_) => {
            println!("{} killed", cmd);
        }
        Err(err) => {
            println!("failed to kill {}: {}", cmd, err);
        }
    }
    println!("reach exit");
    exit(exit_code)
}

fn check_previlige() {
    #[cfg(unix)]
    match sudo::escalate_if_needed() {
        Ok(_) => {}
        Err(_) => {
            println!("please run as root");
            exit(EPERM);
        }
    }

    #[cfg(windows)]
    if !is_elevated::is_elevated() {
        println!("please run as administrator");
        exit(EPERM);
    }
}

fn print_version() {
    let pkg_name = env!("CARGO_PKG_NAME");
    let pkg_version = env!("CARGO_PKG_VERSION");
    println!("running {}@{}", pkg_name, pkg_version);
}
