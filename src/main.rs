mod api;
mod client;
mod config;
mod resp;
mod state;
mod template;
mod utils;
mod wg;

use std::{env, path::Path, process::exit};

use client::Client;
use config::{Config, WgConf};
use wg::{gen_wg_conf, start_wg_quick, stop_wg_quick};

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

#[tokio::main]
async fn main() {
    print_version();
    return;
    let conf_file = parse_arg();
    let conf = Config::from_file(&conf_file).await;
    let name = conf.conf_name.clone();
    let conf_dir = conf.conf_dir.clone();
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
    tokio::select! {
        // keep alive
        _ = c.keep_alive_vpn(&wg_conf, 60)=>{},

        // start service and handle signal
        _ = async {
            let conf_path = Path::new(&conf_dir).join(format!("{}.conf", name));
            println!("generating config to {}", &conf_path.to_str().unwrap());
            match gen_wg_conf(&conf_path, &wg_conf).await {
                Ok(_) => {},
                Err(e) => {
                    println!("failed to generate wg conf: {}",e);
                    return;
                },
            }
            // no error because user can start it manually
            println!("start {}", utils::service_name(&name));
            start_wg_quick(&name).await;
            started = true;

            match tokio::signal::ctrl_c().await {
                Ok(_) => {},
                Err(e) => {
                    println!("failed to receive signal: {}",e);
                },
            }
            println!("ctrl+v received");
        }=>{
            println!("exiting...");
        },
    }

    // shutdown
    println!("disconnecting...");
    match c.disconnect_vpn(&wg_conf).await {
        Ok(_) => {}
        Err(e) => println!("failed to disconnect vpn: {}", e),
    };
    if started {
        println!("stopping service...");
        stop_wg_quick(&name).await;
        println!("stopped")
    }
    println!("exited");
}

fn print_version() {
    let pkg_name = env!("CARGO_PKG_NAME");
    let pkg_version = env!("CARGO_PKG_VERSION");
    println!("running {}@{}", pkg_name, pkg_version);
}
