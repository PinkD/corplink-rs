mod api;
mod client;
mod config;
mod resp;
mod state;
mod template;
mod totp;
mod utils;
mod wg;

use std::{env, path::Path, process::exit};

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
    let name = args.nth(0).unwrap();
    match args.len() {
        0 => {}
        1 => {
            // pop arg
            let arg = args.nth(0).unwrap();
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
    return conf_file;
}

pub const ETIMEDOUT: i32 = 110;

#[tokio::main]
async fn main() {
    print_version();
    let conf_file = parse_arg();
    let mut conf = Config::from_file(&conf_file).await;
    let name = conf.conf_name.clone().unwrap();
    let conf_dir = conf.conf_dir.clone().unwrap();
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
                exit(1);
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
    let wg_conf = wg_conf.unwrap();
    let mut started = false;
    let mut exit_code = 0;
    tokio::select! {
        // start service and handle signal
        _ = async {
            let conf_path = Path::new(&conf_dir).join(format!("{}.conf", name));
            println!("generating config to {}", &conf_path.to_str().unwrap());
            match wg::gen_wg_conf(&conf_path, &wg_conf).await {
                Ok(_) => {},
                Err(e) => {
                    println!("failed to generate wg conf: {}",e);
                    return;
                },
            }
            // no error because user can start it manually
            println!("start {}", utils::service_name(&name));
            wg::start_wg_quick(&name).await;
            started = true;

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
            wg::check_wg_connection(&name).await;
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
    if started {
        println!("stopping service...");
        wg::stop_wg_quick(&name).await;
        println!("stopped")
    }
    println!("exited");
    exit(exit_code)
}

fn print_version() {
    let pkg_name = env!("CARGO_PKG_NAME");
    let pkg_version = env!("CARGO_PKG_VERSION");
    println!("running {}@{}", pkg_name, pkg_version);
}
