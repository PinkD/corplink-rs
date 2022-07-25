use std::fmt;
use tokio::fs;

use serde::{Deserialize, Serialize};

use crate::state::State;
use crate::utils;

const DEFAULT_DEVICE_NAME: &str = "DollarOS";
const DEFAULT_INTERFACE_NAME: &str = "corplink";
pub const DEFAULT_CMD_WG_NAME: &str = "wg-corplink";
pub const PLATFORM_LDAP: &str = "ldap";

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub company_name: String,
    pub username: String,
    pub password: Option<String>,
    pub platform: Option<String>,
    pub code: Option<String>,
    pub device_name: Option<String>,
    pub device_id: Option<String>,
    pub public_key: Option<String>,
    pub private_key: Option<String>,
    pub server: Option<String>,
    pub interface_name: Option<String>,
    pub wg_binary: Option<String>,
    #[serde(skip_serializing)]
    pub conf_file: Option<String>,
    pub state: Option<State>,
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let s = serde_json::to_string_pretty(self).unwrap();
        return write!(f, "{}", s);
    }
}

impl Config {
    pub async fn from_file(file: &str) -> Config {
        let conf_str = fs::read_to_string(file)
            .await
            .unwrap_or_else(|e| panic!("failed to read config file {}: {}", file, e));

        let mut conf: Config = serde_json::from_str(&conf_str[..])
            .unwrap_or_else(|e| panic!("failed to parse config file {}: {}", file, e));

        conf.conf_file = Some(file.to_string());
        let mut update_conf = false;
        if conf.interface_name == None {
            conf.interface_name = Some(DEFAULT_INTERFACE_NAME.to_string());
            update_conf = true;
        }
        if conf.wg_binary != None {
            println!("using wg binary {}", conf.wg_binary.clone().unwrap());
        }
        if conf.device_name == None {
            conf.device_name = Some(DEFAULT_DEVICE_NAME.to_string());
            update_conf = true;
        }
        if conf.device_id == None {
            conf.device_id = Some(format!(
                "{:x}",
                md5::compute(&conf.device_name.clone().unwrap())
            ));
            update_conf = true;
        }
        match &conf.private_key {
            Some(private_key) => match conf.public_key {
                Some(_) => {
                    // both keys exist, do nothing
                }
                None => {
                    // only private key exists, generate public from private
                    let public_key = utils::gen_public_key_from_private(private_key).unwrap();
                    conf.public_key = Some(public_key);
                    update_conf = true;
                }
            },
            None => {
                // no key exists, generate new
                let (public_key, private_key) = utils::gen_wg_keypair();
                (conf.public_key, conf.private_key) = (Some(public_key), Some(private_key));
                update_conf = true;
            }
        }
        if update_conf {
            conf.save().await;
        }
        conf
    }

    pub async fn save(&self) {
        let file = self.conf_file.as_ref().unwrap();
        let data = format!("{}", &self);
        fs::write(file, data).await.unwrap();
    }
}

#[derive(Serialize, Clone)]
pub struct WgConf {
    // standard wg conf
    pub address: String,
    pub mask: u32,
    pub peer_address: String,
    pub mtu: u32,
    pub public_key: String,
    pub private_key: String,
    pub peer_key: String,
    pub route: Vec<String>,

    // extent confs
    pub dns: String,

    // corplink confs
    pub protocol_version: String,
    pub protocol: i32,
}
