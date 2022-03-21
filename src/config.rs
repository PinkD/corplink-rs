use std::fmt;
use tokio::fs;

use serde::{Deserialize, Serialize};
use serde_json;

use crate::state::State;

#[derive(Serialize, Deserialize, Clone)]
pub struct Config {
    pub username: String,
    pub password: Option<String>,
    pub code: Option<String>,
    pub device_name: String,
    pub device_id: Option<String>,
    pub public_key: String,
    pub private_key: String,
    pub server: String,
    pub conf_name: String,
    pub conf_dir: String,
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
        let conf_str = fs::read_to_string(file).await
            .unwrap_or_else(|e| panic!("failed to read config file {}: {}", file, e));

        let mut conf: Config = serde_json::from_str(&conf_str[..])
            .unwrap_or_else(|e| panic!("failed to parse config file {}: {}", file, e));

        conf.conf_file = Some(file.to_string());
        if conf.device_id == None {
            conf.device_id = Some(format!("{:x}", md5::compute(&conf.device_name)))
        }
        // TODO: debug
        println!("{}", &conf);
        return conf;
    }

    pub async fn save(&self) {
        fs::write(&self.conf_file.as_ref().unwrap(), format!("{}", &self)).await.unwrap();
    }
}

#[derive(Serialize, Clone)]
pub struct WgConf {
    pub address: String,
    pub mask: u32,
    pub peer_address: String,
    pub mtu: u32,
    pub public_key: String,
    pub private_key: String,
    pub peer_key: String,
    // ", ".join(Vec<String>)
    pub route: String,
}

pub const WG_CONF_TEMPLATE: &str = "[Interface]
Address = {{address}}/{{mask}}
# PublicKey = {{public_key}}
PrivateKey = {{private_key}}
MTU = {{mtu}}

[Peer]
PublicKey = {{peer_key}}
AllowedIPs = {{route}}
Endpoint = {{peer_address}}
PersistentKeepalive = 10
";
