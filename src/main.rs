mod api;
mod client;
mod config;
mod qrcode;
mod resp;
mod state;
mod template;
mod totp;
mod utils;
mod wg;

#[cfg(windows)]
use is_elevated;

use env_logger::{Builder, Env, Target};
use std::env;
use std::process::exit;

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
    Builder::from_env(Env::default().default_filter_or("info"))
        .target(Target::Stdout)
        .init();

    print_version();
    check_previlige();

    let conf_file = parse_arg();
    let mut conf = Config::from_file(&conf_file).await;
    let name = conf.interface_name.clone().unwrap();

    match conf.server {
        Some(_) => {}
        None => match client::get_company_url(conf.company_name.as_str()).await {
            Ok(resp) => {
                log::info!(
                    "company name is {}(zh)/{}(en) server is {}",
                    resp.zh_name,
                    resp.en_name,
                    resp.domain
                );
                conf.server = Some(resp.domain);
                conf.save().await;
            }
            Err(err) => {
                log::error!(
                    "failed to fetch company server from company name {}: {}",
                    conf.company_name,
                    err
                );
                exit(EPERM);
            }
        },
    }

    let with_wg_log = conf.debug_wg.unwrap_or_default();
    let mut c = Client::new(conf).unwrap();
    let mut logout_retry = true;
    let wg_conf: Option<WgConf>;

    loop {
        if c.need_login() {
            log::info!("not login yet, try to login");
            c.login().await.unwrap();
            log::info!("login success");
        }
        log::info!("try to connect");
        match c.connect_vpn().await {
            Ok(conf) => {
                wg_conf = Some(conf);
                break;
            }
            Err(e) => {
                if logout_retry && e.to_string().contains("logout") {
                    // e contains detail message, so just print it out
                    log::warn!("{}", e);
                    logout_retry = false;
                    continue;
                } else {
                    panic!("{}", e);
                }
            }
        };
    }
    log::info!("start wg-corplink for {}", &name);
    let wg_conf = wg_conf.unwrap();
    let protocol = wg_conf.protocol;
    if !wg::start_wg_go(&name, protocol, with_wg_log) {
        log::warn!("failed to start wg-corplink for {}", name);
        exit(EPERM);
    }
    let mut uapi = wg::UAPIClient { name: name.clone() };
    match uapi.config_wg(&wg_conf).await {
        Ok(_) => {}
        Err(err) => {
            log::error!("failed to config interface with uapi for {}: {}", name, err);
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
                    log::warn!("failed to receive signal: {}",e);
                },
            }
            log::info!("ctrl+v received");
        } => {},

        // keep alive
        _ = c.keep_alive_vpn(&wg_conf, 60) => {
            exit_code = ETIMEDOUT;
        },

        // check wg handshake and exit if timeout
        _ = async {
            uapi.check_wg_connection().await;
            log::warn!("last handshake timeout");
        } => {
            exit_code = ETIMEDOUT;
        },
    }

    // shutdown
    log::info!("disconnecting vpn...");
    match c.disconnect_vpn(&wg_conf).await {
        Ok(_) => {}
        Err(e) => log::warn!("failed to disconnect vpn: {}", e),
    };

    wg::stop_wg_go();

    log::info!("reach exit");
    exit(exit_code)
}

fn check_previlige() {
    #[cfg(unix)]
    match sudo::escalate_if_needed() {
        Ok(_) => {}
        Err(_) => {
            log::error!("please run as root");
            exit(EPERM);
        }
    }

    #[cfg(windows)]
    if !is_elevated::is_elevated() {
        log::error!("please run as administrator");
        exit(EPERM);
    }
}

fn print_version() {
    let pkg_name = env!("CARGO_PKG_NAME");
    let pkg_version = env!("CARGO_PKG_VERSION");
    log::info!("running {}@{}", pkg_name, pkg_version);
}
