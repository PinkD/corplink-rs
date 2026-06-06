mod api;
mod client;
mod config;
mod dns;
mod qrcode;
mod resp;
mod state;
mod template;
mod totp;
mod utils;
mod wg;

#[cfg(windows)]
use is_elevated;

#[cfg(any(target_os = "macos", target_os = "linux"))]
use dns::DNSManager;

use std::env;
use std::process::exit;

use anyhow::{anyhow, Context, Result};

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
    if let Err(err) = run().await {
        log::error!("{:#}", err);
        exit(EPERM);
    }
}

async fn run() -> Result<()> {
    // NOTE: If you want to debug, you should set `RUST_LOG` env to `debug` and run corplink-rs in root
    //  because `check_privilege` will call sudo and drop env if you're not root
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    print_version();

    let conf_file = parse_arg();
    let mut conf = Config::from_file(&conf_file)
        .await
        .context("failed to load config")?;
    let name = conf
        .interface_name
        .clone()
        .context("interface name missing in config")?;
    let socks5_listen = conf.socks5_listen.clone();
    let socks5_username = conf.socks5_username.clone().unwrap_or_default();
    let socks5_password = conf.socks5_password.clone().unwrap_or_default();
    let netstack_mode = socks5_listen.is_some();

    // netstack/socks5 mode runs entirely in userspace (no kernel TUN device,
    // no system routes/dns), so it does not require elevated privileges.
    if !netstack_mode {
        check_privilege();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let use_vpn_dns = conf.use_vpn_dns.unwrap_or(false);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let dns_backup_filename = conf.dns_backup_filename.clone();

    if conf.server.is_none() {
        let resp = client::get_company_url(conf.company_name.as_str())
            .await
            .with_context(|| {
                format!(
                    "failed to fetch company server from company name {}",
                    conf.company_name
                )
            })?;
        log::info!(
            "company name is {}(zh)/{}(en) server is {}",
            resp.zh_name,
            resp.en_name,
            resp.domain
        );
        conf.server = Some(resp.domain);
        conf.save()
            .await
            .context("failed to persist company server")?;
    }

    let with_wg_log = conf.debug_wg.unwrap_or_default();
    let platform = conf.platform.clone();
    let mut c = Client::new(conf).context("failed to initialize client")?;
    let mut logout_retry = true;
    let wg_conf: Option<WgConf>;

    loop {
        if c.need_login() {
            log::info!("not login yet, try to login");
            c.login().await.context("login failed")?;
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
                    return Err(e);
                }
            }
        };
    }
    let wg_conf = wg_conf.ok_or_else(|| anyhow!("wg conf missing after connect loop"))?;
    let protocol = wg_conf.protocol;
    let mut uapi = wg::UAPIClient { name: name.clone() };
    if let Some(listen) = &socks5_listen {
        log::info!("start wg-corplink (netstack/socks5) on {}", listen);
        wg::start_wg_go_netstack(&wg_conf, listen, &socks5_username, &socks5_password, with_wg_log)
            .context("failed to start wg-corplink in netstack mode")?;
        uapi.config_wg_netstack(&wg_conf)
            .await
            .context("failed to config netstack interface with uapi")?;
        if socks5_username.is_empty() {
            log::info!("socks5 proxy ready at {} (no auth)", listen);
        } else {
            log::info!(
                "socks5 proxy ready at {} (username/password auth required)",
                listen
            );
        }
    } else {
        log::info!("start wg-corplink for {}", &name);
        wg::start_wg_go(&name, protocol, with_wg_log)
            .with_context(|| format!("failed to start wg-corplink for {}", name))?;
        uapi.config_wg(&wg_conf)
            .await
            .with_context(|| format!("failed to config interface with uapi for {name}"))?;
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    let mut dns_manager = DNSManager::new(dns_backup_filename);

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if use_vpn_dns && !netstack_mode {
        match dns_manager.set_dns(vec![&wg_conf.dns], vec![]) {
            Ok(_) => {}
            Err(err) => {
                log::warn!("failed to set dns: {}", err);
            }
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
            log::info!("ctrl+c received");
        } => {},

        // keep alive
        // _ = c.keep_alive_vpn(&wg_conf, 60) => {
        //     exit_code = ETIMEDOUT;
        // },

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
    if let Err(e) = c.disconnect_vpn(&wg_conf).await {
        log::warn!("failed to disconnect vpn: {}", e)
    };

    // only logout for feilian_v1
    if platform.as_deref() == Some(config::PLATFORM_CORPLINK_V1) {
        log::info!("logging out current terminal...");
        if let Err(e) = c.logout().await {
            log::warn!("failed to logout: {}", e)
        };
    }

    wg::stop_wg_go();

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if use_vpn_dns && !netstack_mode {
        match dns_manager.restore_dns() {
            Ok(_) => {}
            Err(err) => {
                log::warn!("failed to delete dns: {}", err);
            }
        }
    }

    log::info!("reach exit");
    exit(exit_code)
}

fn check_privilege() {
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
