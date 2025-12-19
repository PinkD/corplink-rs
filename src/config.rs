use std::fmt;
use tokio::fs;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::state::State;
use crate::utils;

const DEFAULT_DEVICE_NAME: &str = "DollarOS";
const DEFAULT_INTERFACE_NAME: &str = "corplink";

pub const PLATFORM_LDAP: &str = "ldap";
pub const PLATFORM_CORPLINK: &str = "feilian";
pub const PLATFORM_OIDC: &str = "OIDC";
// aka feishu
pub const PLATFORM_LARK: &str = "lark";
#[allow(dead_code)]
pub const PLATFORM_WEIXIN: &str = "weixin";
// aka dingding
#[allow(dead_code)]
pub const PLATFORM_DING_TALK: &str = "dingtalk";
// unknown
#[allow(dead_code)]
pub const PLATFORM_AAD: &str = "aad";

pub const STRATEGY_LATENCY: &str = "latency";
pub const STRATEGY_DEFAULT: &str = "default";

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
    pub debug_wg: Option<bool>,
    #[serde(skip_serializing)]
    pub conf_file: Option<String>,
    pub state: Option<State>,
    pub vpn_server_name: Option<String>,
    pub vpn_select_strategy: Option<String>,
    pub use_vpn_dns: Option<bool>,
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match serde_json::to_string_pretty(self) {
            Ok(s) => write!(f, "{}", s),
            Err(e) => write!(f, "<invalid config: {e}>")
        }
    }
}

impl Config {
    pub async fn from_file(file: &str) -> Result<Config> {
        let conf_str = fs::read_to_string(file)
            .await
            .with_context(|| format!("failed to read config file {file}"))?;

        let mut conf: Config = serde_json::from_str(&conf_str[..])
            .with_context(|| format!("failed to parse config file {file}"))?;

        conf.conf_file = Some(file.to_string());
        let mut update_conf = false;
        if conf.interface_name.is_none() {
            conf.interface_name = Some(DEFAULT_INTERFACE_NAME.to_string());
            update_conf = true;
        }
        if conf.device_name.is_none() {
            conf.device_name = Some(DEFAULT_DEVICE_NAME.to_string());
            update_conf = true;
        }
        if conf.device_id.is_none() {
            let device_name = conf
                .device_name
                .as_ref()
                .context("device name missing when generating device id")?;
            conf.device_id = Some(format!("{:x}", md5::compute(device_name)));
            update_conf = true;
        }
        match &conf.private_key {
            Some(private_key) => match conf.public_key {
                Some(_) => {
                    // both keys exist, do nothing
                }
                None => {
                    // only private key exists, generate public from private
                    let public_key = utils::gen_public_key_from_private(private_key)?;
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
            conf.save().await?;
        }
        Ok(conf)
    }

    pub async fn save(&self) -> Result<()> {
        let file = self
            .conf_file
            .as_ref()
            .context("config file path missing")?;
        let data = format!("{}", &self);
        fs::write(file, data)
            .await
            .with_context(|| format!("failed to write config file {file}"))?;
        Ok(())
    }
}

#[derive(Serialize, Clone)]
pub struct WgConf {
    // standard wg conf
    pub address: String,
    pub address6: String,
    pub peer_address: String,
    pub mtu: u32,
    pub public_key: String,
    pub private_key: String,
    pub peer_key: String,
    pub route: Vec<String>,

    // extra confs
    pub dns: String,

    // corplink confs
    pub protocol: i32,
}
