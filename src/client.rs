use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use std::{fs, io};

use basic_otp::totp;
use cookie_store::{Cookie, CookieStore};
use reqwest::{ClientBuilder, Url};
use reqwest_cookie_store::CookieStoreMutex;
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};
use sha2::Digest;

use crate::api::{ApiName, ApiUrl};
use crate::config::{Config, WgConf};
use crate::resp::{Resp, RespLogin, RespLoginMethod, RespVpnInfo, RespWgInfo};
use crate::state::State;
use crate::utils;

const COOKIE_FILE: &str = "cookies.json";
const USER_AGENT: &str = "CorpLink/20500 (GooglePixel; Android 10; en)";

#[derive(Debug)]
pub enum Error {
    ReqwestError(reqwest::Error),
    Error(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::ReqwestError(err) => {
                return err.fmt(f);
            }
            Error::Error(err) => {
                return write!(f, "{}", err);
            }
        }
    }
}

#[derive(Clone)]
pub struct Client {
    conf: Config,
    cookie: Arc<CookieStoreMutex>,
    c: reqwest::Client,
    api_url: ApiUrl,
}

unsafe impl Send for Client {}

unsafe impl Sync for Client {}

impl Client {
    pub fn new(conf: Config) -> Result<Client, Error> {
        let cookie_store = {
            let file = fs::File::open(COOKIE_FILE).map(io::BufReader::new);
            match file {
                Ok(file) => CookieStore::load_json(file).unwrap(),
                Err(_) => CookieStore::default(),
            }
        };
        let cookie_store = Arc::new(CookieStoreMutex::new(cookie_store));

        let c = ClientBuilder::new()
            // alow invalid certs because this cert is signed by corplink
            .danger_accept_invalid_certs(true)
            .user_agent(USER_AGENT)
            .cookie_provider(Arc::clone(&cookie_store))
            .build();
        if let Err(err) = c {
            return Err(Error::ReqwestError(err));
        }
        let conf_bak = conf.clone();
        let c = c.unwrap();
        return Ok(Client {
            conf,
            cookie: Arc::clone(&cookie_store),
            c,
            api_url: ApiUrl::new(&conf_bak),
        });
    }

    async fn change_state(&mut self, state: State) {
        self.conf.state = Some(state);
        self.conf.save().await;
    }

    fn save_cookie(&self) {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(false)
            .open(COOKIE_FILE)
            .map(io::BufWriter::new)
            .unwrap();
        let c = self.cookie.lock().unwrap();
        c.save_json(&mut file).unwrap();
    }

    async fn request<T: DeserializeOwned>(
        &mut self,
        api: ApiName,
        body: Option<Map<String, Value>>,
    ) -> Result<Resp<T>, Error> {
        let url = self.api_url.get_api_url(&api);
        let resp = match body {
            Some(body) => {
                let body = serde_json::to_string(&body).unwrap();
                self.c.post(url).body(body).send().await
            }
            None => self.c.get(url).send().await,
        };

        if let Err(err) = resp {
            return Err(Error::ReqwestError(err));
        }
        let resp = resp.unwrap();

        for (name, _) in resp.headers() {
            if name.to_string().to_lowercase() == "set-cookie" {
                println!("found set-cookie in header, saving cookie");
                self.save_cookie();
                break;
            }
        }
        let resp = resp.json::<Resp<T>>().await;
        if let Err(err) = resp {
            return Err(Error::ReqwestError(err));
        }
        Ok(resp.unwrap())
    }

    async fn request_imut<T: DeserializeOwned>(
        &self,
        api: ApiName,
        body: Option<Map<String, Value>>,
    ) -> Result<Resp<T>, Error> {
        let url = self.api_url.get_api_url(&api);
        let resp = match body {
            Some(body) => {
                let body = serde_json::to_string(&body).unwrap();
                self.c.post(url).body(body).send().await
            }
            None => self.c.get(url).send().await,
        };

        if let Err(err) = resp {
            return Err(Error::ReqwestError(err));
        }
        let resp = resp.unwrap();
        let resp = resp.json::<Resp<T>>().await;
        if let Err(err) = resp {
            return Err(Error::ReqwestError(err));
        }
        Ok(resp.unwrap())
    }

    pub fn need_login(&self) -> bool {
        return self.conf.state == None || self.conf.state.as_ref().unwrap() == &State::Init;
    }

    pub async fn login(&mut self) -> Result<(), Error> {
        let method = self.get_login_method().await?;
        for auth in method.auth {
            let otp_uri = match auth.as_str() {
                "password" => {
                    let mut otp = String::new();
                    if let Some(password) = &self.conf.password {
                        if !password.is_empty() {
                            println!("try to login with password");
                            otp = self.login_with_password().await?;
                        }
                    }
                    if otp.is_empty() {
                        println!("no password provided, fallback to email login");
                        continue;
                    }
                    otp
                }
                "email" => {
                    println!("try to login with code from email");
                    self.login_with_email().await?
                }
                _ => panic!("failed to get otp uri"),
            };
            self.change_state(State::Login).await;

            let url = Url::parse(&otp_uri).unwrap();
            for (k, v) in url.query_pairs() {
                if k == "secret" {
                    println!("got 2fa token: {}", &v);
                    self.conf.code = Some(v.to_string());
                    self.conf.save().await;
                    break;
                }
            }

            if let Some(code) = &self.conf.code {
                if !code.is_empty() {
                    return Ok(());
                }
            }
            println!("failed to get otp code");
            return Ok(());
        }
        panic!("no available login method");
    }

    async fn get_login_method(&mut self) -> Result<RespLoginMethod, Error> {
        let mut m = Map::new();
        m.insert("forget_password".to_string(), json!(false));
        m.insert("user_name".to_string(), json!(&self.conf.username));

        let resp = self
            .request::<RespLoginMethod>(ApiName::LoginMethod, Some(m))
            .await?;
        Ok(resp.data.unwrap())
    }

    async fn login_with_password(&mut self) -> Result<String, Error> {
        let mut password = self.conf.password.as_ref().unwrap().clone();
        if password.len() != 64 {
            let mut sha = sha2::Sha256::new();
            sha.update(password.as_bytes());
            password = format!("{:x}", sha.finalize());
        } // else: password already convert to sha256sum
        let mut m = Map::new();
        m.insert("forget_password".to_string(), json!(false));
        m.insert("password".to_string(), json!(password));
        m.insert("user_name".to_string(), json!(&self.conf.username));

        let resp = self
            .request::<RespLogin>(ApiName::LoginPassword, Some(m))
            .await?;
        match resp.code {
            0 => Ok(resp.data.unwrap().url),
            _ => {
                let msg = resp.message.unwrap();
                Err(Error::Error(msg))
            }
        }
    }

    async fn request_email_code(&mut self) -> Result<(), Error> {
        let mut m = Map::new();
        m.insert("forget_password".to_string(), json!(false));
        m.insert("code_type".to_string(), json!("email"));
        m.insert("user_name".to_string(), json!(&self.conf.username));

        self.request::<Map<String, Value>>(ApiName::RequestEmailCode, Some(m))
            .await?;
        Ok(())
    }

    async fn login_with_email(&mut self) -> Result<String, Error> {
        // tell server to send code to email
        println!("try to request code for email");
        self.request_email_code().await?;

        println!("input your code from email:");
        let input = utils::read_line().await;
        let code = input.trim();
        let mut m = Map::new();
        m.insert("forget_password".to_string(), json!(false));
        m.insert("code_type".to_string(), json!("email"));
        m.insert("code".to_string(), json!(code));

        let resp = self
            .request::<RespLogin>(ApiName::LoginEmail, Some(m))
            .await?;
        match resp.code {
            0 => Ok(resp.data.unwrap().url),
            _ => Err(Error::Error(format!(
                "failed to login with email code {}: {}",
                code,
                resp.message.unwrap()
            ))),
        }
    }

    async fn handle_logout_err(&mut self, msg: String) -> Error {
        self.change_state(State::Init).await;
        Error::Error(format!("operation failed because of logout: {}", msg))
    }

    async fn list_vpn(&mut self) -> Result<Vec<RespVpnInfo>, Error> {
        let resp = self
            .request::<Vec<RespVpnInfo>>(ApiName::ListVPN, None)
            .await?;
        match resp.code {
            0 => Ok(resp.data.unwrap()),
            101 => Err(self.handle_logout_err(resp.message.unwrap()).await),
            _ => Err(Error::Error(format!(
                "failed to list vpn with error {}: {}",
                resp.code,
                resp.message.unwrap()
            ))),
        }
    }

    async fn ping_vpn(&mut self, ip: String, api_port: u16) -> bool {
        // config cookie
        let mut cookie = self.cookie.lock().unwrap();
        let mut domain = self.conf.server.clone();
        if domain.contains(":") {
            domain = domain.split(":").collect::<Vec<&str>>()[0].to_string();
        }
        domain = format!("https://{}", domain);

        let mut cookies: Vec<Cookie> = Vec::new();
        for c in cookie.iter_any() {
            if c.domain.matches(&Url::from_str(&domain).unwrap()) {
                cookies.push(c.clone());
            }
        }
        domain = format!("https://{}", &ip);
        for c in cookies {
            let url = &Url::from_str(&domain).unwrap();
            let mut c = cookie::Cookie::new(c.name().to_string(), c.value().to_string());
            c.set_domain(ip.clone());
            let c = Cookie::try_from_raw_cookie(&c, url).unwrap();
            cookie.insert(c, url).unwrap();
        }
        drop(cookie);

        self.api_url.vpn_param.ip = ip.clone();
        self.api_url.vpn_param.port = api_port;
        let result = self.request::<String>(ApiName::PingVPN, None).await;
        match result {
            Ok(resp) => match resp.code {
                0 => return true,
                _ => {
                    println!(
                        "failed to ping vpn with error {}: {}",
                        resp.code,
                        resp.message.unwrap()
                    );
                }
            },
            Err(err) => {
                println!("failed to ping {}:{}: {}", ip, api_port, err);
            }
        }
        return false;
    }

    async fn fetch_peer_info(&mut self, public_key: &String) -> Result<RespWgInfo, Error> {
        let mut otp = String::new();
        if let Some(code) = &self.conf.code {
            if !code.is_empty() {
                let code = utils::b32_decode(code);
                let raw_otp = totp(code.as_slice());
                otp = format!("{:06}", raw_otp);
                println!("2fa code generated: {}", &otp);
            }
        }
        if otp.is_empty() {
            println!("input your 2fa code:");
            otp = utils::read_line().await;
        }
        let mut m = Map::new();
        m.insert("public_key".to_string(), json!(public_key));
        m.insert("otp".to_string(), json!(otp));
        let resp = self
            .request::<RespWgInfo>(ApiName::ConnectVPN, Some(m))
            .await?;
        match resp.code {
            0 => Ok(resp.data.unwrap()),
            101 => Err(self.handle_logout_err(resp.message.unwrap()).await),
            _ => Err(Error::Error(format!(
                "failed to fetch peer info with error {}: {}",
                resp.code,
                resp.message.unwrap()
            ))),
        }
    }

    pub async fn connect_vpn(&mut self) -> Result<WgConf, Error> {
        let vpn_info = self.list_vpn().await?;
        let mut avalaible = false;

        println!("found {} vpn(s)", vpn_info.len());
        let mut vpn_addr = String::new();
        for vpn in vpn_info {
            println!("check if {}:{} is available", &vpn.ip, &vpn.vpn_port);
            vpn_addr = format!("{}:{}", &vpn.ip, vpn.vpn_port);
            if self.ping_vpn(vpn.ip, vpn.api_port).await {
                println!("available");
                avalaible = true;
                break;
            }
            println!("not available");
        }
        if !avalaible {
            return Err(Error::Error("no vpn available".to_string()));
        }

        let key = self.conf.public_key.clone().unwrap();
        println!("try to get wg conf from remote");
        let wg_info = self.fetch_peer_info(&key).await?;
        let mtu = wg_info.setting.vpn_mtu;
        let dns = wg_info.setting.vpn_dns;
        let peer_key = wg_info.public_key;
        let public_key = self.conf.public_key.clone().unwrap();
        let private_key = self.conf.private_key.clone().unwrap();
        let mut route = wg_info.setting.vpn_route_split;
        // remove dns because it's useless
        if route.len() != 0 && route[0].starts_with(&dns) {
            route.remove(0);
        }
        let route = route.join(", ");
        let wg_conf = WgConf {
            address: wg_info.ip,
            mask: wg_info.ip_mask.parse::<u32>().unwrap(),
            peer_address: vpn_addr,
            mtu,
            public_key,
            private_key,
            peer_key,
            route,
        };
        Ok(wg_conf)
    }

    pub async fn keep_alive_vpn(&self, conf: &WgConf, interval: u64) -> Result<(), Error> {
        loop {
            println!("keep alive");
            match self.keep_alive_vpn_internal(&conf).await {
                Ok(_) => (),
                Err(err) => return Err(Error::Error(format!("keep alive error: {}", err))),
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    }

    async fn keep_alive_vpn_internal(&self, conf: &WgConf) -> Result<(), Error> {
        let mut m = Map::new();
        m.insert("ip".to_string(), json!(conf.address));
        m.insert("public_key".to_string(), json!(conf.public_key));
        m.insert("mode".to_string(), json!("Split"));
        m.insert("type".to_string(), json!("100"));

        let resp = self
            .request_imut::<Map<String, Value>>(ApiName::KeepAliveVPN, Some(m))
            .await?;
        match resp.code {
            0 => Ok(()),
            _ => Err(Error::Error(format!(
                "failed to fetch peer info with error {}: {}",
                resp.code,
                resp.message.unwrap()
            ))),
        }
    }

    pub async fn disconnect_vpn(&self, wg_conf: &WgConf) -> Result<(), Error> {
        let mut m = Map::new();
        m.insert("ip".to_string(), json!(wg_conf.address));
        m.insert("public_key".to_string(), json!(wg_conf.public_key));
        m.insert("mode".to_string(), json!("Split"));
        m.insert("type".to_string(), json!("101"));
        let resp = self
            .request_imut::<Map<String, Value>>(ApiName::DisconnectVPN, Some(m))
            .await?;
        match resp.code {
            0 => Ok(()),
            _ => Err(Error::Error(format!(
                "failed to fetch peer info with error {}: {}",
                resp.code,
                resp.message.unwrap()
            ))),
        }
    }
}
