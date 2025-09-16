use chrono::Utc;
use std::collections::HashMap;
use std::fmt;
use std::path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use std::{fs, io};

use cookie::Cookie as RawCookie;
use cookie_store::{Cookie, CookieStore};
use reqwest::header;
use reqwest::{ClientBuilder, Response, Url};
use reqwest_cookie_store::CookieStoreMutex;
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};
use sha2::Digest;

use crate::api::{ApiName, ApiUrl, URL_GET_COMPANY};
use crate::config::{
    Config, WgConf, PLATFORM_CORPLINK, PLATFORM_LARK, PLATFORM_LDAP, PLATFORM_OIDC,
    STRATEGY_DEFAULT, STRATEGY_LATENCY,
};
use crate::qrcode::TerminalQrCode;
use crate::resp::*;
use crate::state::State;
use crate::totp::{totp_offset, TIME_STEP};
use crate::utils;

const COOKIE_FILE_SUFFIX: &str = "cookies.json";
const USER_AGENT: &str = "CorpLink/201000 (GooglePixel; Android 10; en)";

#[derive(Debug)]
pub enum Error {
    ReqwestError(reqwest::Error),
    Error(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::ReqwestError(err) => err.fmt(f),
            Error::Error(err) => {
                write!(f, "{}", err)
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
    date_offset_sec: i32,
}

unsafe impl Send for Client {}

unsafe impl Sync for Client {}

pub async fn get_company_url(code: &str) -> Result<RespCompany, Error> {
    let c = ClientBuilder::new()
        // allow invalid certs because this cert is signed by corplink
        .danger_accept_invalid_certs(true)
        .build();
    if let Err(err) = c {
        return Err(Error::ReqwestError(err));
    }
    let c = c.unwrap();
    let mut m = Map::new();
    m.insert("code".to_string(), json!(code));
    let body = serde_json::to_string(&m).unwrap();

    let resp = c.post(URL_GET_COMPANY).body(body).send().await;
    if let Err(err) = resp {
        return Err(Error::ReqwestError(err));
    }
    let resp = resp.unwrap().json::<Resp<RespCompany>>().await;
    if let Err(err) = resp {
        return Err(Error::ReqwestError(err));
    }
    let resp = resp.unwrap();
    match resp.code {
        0 => Ok(resp.data.unwrap()),
        _ => {
            let msg = resp.message.unwrap();
            Err(Error::Error(msg))
        }
    }
}

impl Client {
    pub fn new(conf: Config) -> Result<Client, Error> {
        let f = conf.conf_file.clone().unwrap();
        let dir = match path::Path::new(&f).parent() {
            Some(dir) => dir,
            None => path::Path::new("."),
        };
        let cookie_file = dir.join(format!(
            "{}_{}",
            conf.interface_name.clone().unwrap(),
            COOKIE_FILE_SUFFIX
        ));
        log::info!("cookie file is: {}", cookie_file.to_str().unwrap());

        let mut cookie_store = {
            let file = fs::File::open(cookie_file).map(io::BufReader::new);
            match file {
                Ok(file) => CookieStore::load_json_all(file).unwrap(),
                Err(_) => CookieStore::default(),
            }
        };
        let has_expired = cookie_store.iter_any().any(|cookie| cookie.is_expired());
        if has_expired {
            log::info!("some cookies are expired");
        }

        let mut headers = header::HeaderMap::new();

        if let Some(server) = &conf.server.clone() {
            let server_url = Url::from_str(server.as_str()).unwrap();

            if let Some(device_id) = &conf.device_id.clone() {
                let _ =
                    cookie_store.insert_raw(&RawCookie::new("device_id", device_id), &server_url);
            }
            if let Some(device_name) = &conf.device_name.clone() {
                let _ = cookie_store
                    .insert_raw(&RawCookie::new("device_name", device_name), &server_url);
            }

            if let Some(domain) = server_url.domain().or_else(|| server_url.host_str()) {
                if let Some(csrf_token) = cookie_store.get(domain, "/", "csrf-token") {
                    headers.insert(
                        "csrf-token",
                        header::HeaderValue::from_str(csrf_token.value()).unwrap(),
                    );
                }
            }
        }

        let cookie_store = Arc::new(CookieStoreMutex::new(cookie_store));

        let c = ClientBuilder::new()
            // allow invalid certs because this cert is signed by corplink
            .danger_accept_invalid_certs(true)
            // for debug
            // .proxy(reqwest::Proxy::all("socks5://192.168.111.233:8001").unwrap())
            .user_agent(USER_AGENT)
            .cookie_provider(Arc::clone(&cookie_store))
            .default_headers(headers)
            .timeout(Duration::from_millis(10000))
            .build();
        if let Err(err) = c {
            return Err(Error::ReqwestError(err));
        }
        let conf_bak = conf.clone();
        let c = c.unwrap();
        Ok(Client {
            conf,
            cookie: Arc::clone(&cookie_store),
            c,
            api_url: ApiUrl::new(&conf_bak),
            date_offset_sec: 0,
        })
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
            .open(format!(
                "{}_{}",
                self.conf.interface_name.clone().unwrap(),
                COOKIE_FILE_SUFFIX
            ))
            .map(io::BufWriter::new)
            .unwrap();
        let c = self.cookie.lock().unwrap();
        c.save_json(&mut file).unwrap();
    }

    async fn request<T: DeserializeOwned + fmt::Debug>(
        &mut self,
        api: ApiName,
        body: Option<Map<String, Value>>,
    ) -> Result<Resp<T>, Error> {
        let url = self.api_url.get_api_url(&api);

        let rb = match body {
            Some(body) => {
                let body = serde_json::to_string(&body).unwrap();
                self.c.post(url).body(body)
            }
            None => self.c.get(url),
        };

        let resp = match rb.send().await {
            Ok(r) => r,
            Err(err) => return Err(Error::ReqwestError(err)),
        };
        // TODO: handle special cases
        if !resp.status().is_success() {
            let msg = format!("logout because of bad resp code: {}", resp.status());
            return Err(self.handle_logout_err(msg).await);
        }

        self.parse_time_offset_from_date_header(&resp);

        for (name, _) in resp.headers() {
            if name.to_string().to_lowercase() == "set-cookie" {
                log::info!("found set-cookie in header, saving cookie");
                self.save_cookie();
                break;
            }
        }
        let resp = resp.json::<Resp<T>>().await;
        if let Err(err) = resp {
            return Err(Error::ReqwestError(err));
        }
        let resp = resp.unwrap();
        log::debug!("api {:#?} resp: {:#?}", api, resp);
        Ok(resp)
    }

    fn parse_time_offset_from_date_header(&mut self, resp: &Response) {
        let headers = resp.headers();
        if headers.contains_key("date") {
            let date = &headers["date"];
            match httpdate::parse_http_date(date.to_str().unwrap()) {
                Ok(date) => {
                    let now = SystemTime::now();
                    self.date_offset_sec = if now < date {
                        let date_offset = date.duration_since(now).unwrap();
                        date_offset.as_secs().try_into().unwrap()
                    } else {
                        let date_offset = now.duration_since(date).unwrap();
                        let offset: i32 = date_offset.as_secs().try_into().unwrap();
                        -offset
                    };
                }
                Err(e) => {
                    log::warn!("failed to parse date in header, ignore it: {}", e);
                }
            }
        }
    }

    pub fn need_login(&self) -> bool {
        return self.conf.state.is_none() || self.conf.state.as_ref().unwrap() == &State::Init;
    }

    async fn check_tps_token(&mut self, token: &String) -> Result<String, Error> {
        // tps confirmed, try to login with token
        let mut m = Map::new();
        m.insert("token".to_string(), json!(token));

        let resp = self
            .request::<RespLogin>(ApiName::TpsTokenCheck, Some(m))
            .await?;
        match resp.code {
            0 => Ok(resp.data.unwrap().url),
            _ => {
                let msg = resp.message.unwrap();
                Err(Error::Error(msg))
            }
        }
    }

    async fn get_otp_uri_from_tps(
        &mut self,
        method: &str,
        url: &String,
        token: &String,
    ) -> Result<String, Error> {
        log::info!("old token is: {token}");
        log::info!("please scan the QR code or visit the following link to auth corplink:\n{url}");
        let code = TerminalQrCode::from_bytes(url.as_bytes());
        code.print();
        match method {
            PLATFORM_LARK | PLATFORM_OIDC => {
                log::info!("press enter if you finish auth");
                let stdin = io::stdin();
                stdin.lines().next();
                self.check_tps_token(token).await
            }
            _ => {
                // TODO: add all tps login support
                panic!("unsupported platform, please contact the developer");
            }
        }
    }

    async fn corplink_login(&mut self) -> Result<String, Error> {
        let resp = self.get_corplink_login_method().await?;
        for method in resp.auth {
            match method.as_str() {
                "password" => {
                    if let Some(password) = &self.conf.password {
                        if !password.is_empty() {
                            log::info!("try to login with password");
                            return self.login_with_password(PLATFORM_CORPLINK).await;
                        }
                    }
                    log::info!("no password provided, trying other methods");
                    continue;
                }
                "email" => {
                    log::info!("try to login with code from email");
                    return self.login_with_email().await;
                }
                _ => {
                    log::info!("unsupported method {method}, trying other methods");
                }
            }
        }
        panic!("failed to login with corplink");
    }

    async fn ldap_login(&mut self) -> Result<String, Error> {
        // I don't know why but we must get login method before login
        let resp = self.get_corplink_login_method().await?;
        for method in resp.auth {
            if method != "password" {
                continue;
            }
            if let Some(password) = &self.conf.password {
                return if !password.is_empty() {
                    self.login_with_password(PLATFORM_LDAP).await
                } else {
                    Err(Error::Error("no password provided".to_string()))
                };
            }
        }
        panic!("failed to login with ldap");
    }

    fn is_platform_or_default(&self, platform: &str) -> bool {
        if let Some(p) = &self.conf.platform {
            return p.is_empty() || platform == p;
        }
        true
    }

    async fn request_otp_code(&mut self) -> Result<String, Error> {
        let m = Map::new();
        let resp = self.request::<RespOtp>(ApiName::OTP, Some(m)).await?;
        match resp.code {
            0 => Ok(resp.data.unwrap().url),
            _ => {
                let msg = resp.message.unwrap();
                Err(Error::Error(msg))
            }
        }
    }

    async fn get_otp_uri_by_otp(
        &mut self,
        tps_login: &HashMap<String, RespTpsLoginMethod>,
        method: &String,
    ) -> Result<String, Error> {
        return match self.get_otp_uri(tps_login, method).await {
            Ok(url) => {
                if url == "" {
                    self.request_otp_code().await
                } else {
                    Ok(url)
                }
            }
            Err(e) => Err(e),
        };
    }
    async fn get_otp_uri(
        &mut self,
        tps_login: &HashMap<String, RespTpsLoginMethod>,
        method: &String,
    ) -> Result<String, Error> {
        if tps_login.contains_key(method) && self.is_platform_or_default(method) {
            log::info!("try to login with third party platform {method}");
            let resp = tps_login.get(method).unwrap();
            return self
                .get_otp_uri_from_tps(method, &resp.login_url, &resp.token)
                .await;
        }
        match method.as_str() {
            PLATFORM_CORPLINK => {
                if self.is_platform_or_default(PLATFORM_CORPLINK) {
                    log::info!("try to login with platform {PLATFORM_CORPLINK}");
                    return self.corplink_login().await;
                }
            }
            PLATFORM_LDAP => {
                if self.is_platform_or_default(PLATFORM_LDAP) {
                    log::info!("try to login with platform {PLATFORM_LDAP}");
                    return self.ldap_login().await;
                }
            }
            _ => {}
        }
        Ok(String::new())
    }

    // choose right login method and login
    pub async fn login(&mut self) -> Result<(), Error> {
        let resp = self.get_login_method().await?;
        let tps_login_resp = self.get_tps_login_method().await?;
        let mut tps_login = HashMap::new();
        for resp in tps_login_resp {
            tps_login.insert(resp.alias.clone(), resp);
        }
        for method in resp.login_orders {
            let otp_uri = self.get_otp_uri_by_otp(&tps_login, &method).await;
            if let Err(e) = otp_uri {
                log::warn!("failed to login with method {method}: {e}");
                continue;
            }
            let otp_uri = otp_uri.unwrap();
            if otp_uri.is_empty() {
                log::warn!("failed to login with method {method}");
                continue;
            }
            self.change_state(State::Login).await;

            let url = Url::parse(&otp_uri).unwrap();
            for (k, v) in url.query_pairs() {
                if k == "secret" {
                    log::info!("got 2fa token: {}", &v);
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
            log::warn!("failed to get otp code");
            return Ok(());
        }
        panic!("no available login method, please provide a valid platform")
    }

    async fn get_login_method(&mut self) -> Result<RespLoginMethod, Error> {
        let resp = self
            .request::<RespLoginMethod>(ApiName::LoginMethod, None)
            .await?;
        Ok(resp.data.unwrap())
    }

    // get 3rd party login methods and links, only lark(feishu) is tested
    async fn get_tps_login_method(&mut self) -> Result<Vec<RespTpsLoginMethod>, Error> {
        let resp = self
            .request::<Vec<RespTpsLoginMethod>>(ApiName::TpsLoginMethod, None)
            .await?;
        Ok(resp.data.unwrap_or_default())
    }

    // get corplink login method, knowing result can be password or email
    async fn get_corplink_login_method(&mut self) -> Result<RespCorplinkLoginMethod, Error> {
        let mut m = Map::new();
        m.insert("forget_password".to_string(), json!(false));
        m.insert("user_name".to_string(), json!(&self.conf.username));

        let resp = self
            .request::<RespCorplinkLoginMethod>(ApiName::CorplinkLoginMethod, Some(m))
            .await?;
        Ok(resp.data.unwrap())
    }

    async fn login_with_password(&mut self, platform: &str) -> Result<String, Error> {
        let mut password = self.conf.password.as_ref().unwrap().clone();
        let mut m = Map::new();
        match platform {
            PLATFORM_LDAP => {
                m.insert("platform".to_string(), json!(PLATFORM_LDAP));
            }
            PLATFORM_CORPLINK => {
                if password.len() != 64 {
                    let mut sha = sha2::Sha256::new();
                    sha.update(password.as_bytes());
                    password = format!("{:x}", sha.finalize());
                } // else: password already convert to sha256sum
            }
            _ => {
                panic!("invalid platform {platform}")
            }
        }
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
        log::info!("try to request code for email");
        self.request_email_code().await?;

        log::info!("input your code from email:");
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

    async fn get_first_vpn_by_latency(
        &mut self,
        vpn_info: Vec<RespVpnInfo>,
    ) -> Option<RespVpnInfo> {
        let mut fast_vpn = None;
        let mut min_latency = i64::MAX;
        for vpn in vpn_info {
            let latency = self.ping_vpn(vpn.ip.clone(), vpn.api_port).await;

            log::info!(
                "server name {}{}",
                vpn.en_name,
                match latency {
                    -1 => " timeout".to_string(),
                    _ => format!(", latency {}ms", latency),
                }
            );
            if latency != -1 && latency < min_latency {
                fast_vpn = Some(vpn);
                min_latency = latency;
            }
        }
        fast_vpn
    }

    async fn get_first_available_vpn(&mut self, vpn_info: Vec<RespVpnInfo>) -> Option<RespVpnInfo> {
        for vpn in vpn_info {
            let latency = self.ping_vpn(vpn.ip.clone(), vpn.api_port.clone()).await;
            if latency != -1 {
                return Some(vpn);
            }
        }
        None
    }

    // ping vpn and return latency in ms. Will return -1 on error
    async fn ping_vpn(&mut self, ip: String, api_port: u16) -> i64 {
        {
            // config cookie
            let mut cookie = self.cookie.lock().unwrap();
            let server_url = self.conf.server.clone().unwrap();

            let mut url = Url::from_str(&server_url).unwrap();
            let mut cookies: Vec<Cookie> = Vec::new();
            for c in cookie.iter_any() {
                if c.domain.matches(&url.clone()) {
                    cookies.push(c.clone());
                }
            }
            url.set_host(Some(ip.as_str())).unwrap();
            url.set_port(Some(api_port)).unwrap();
            for c in cookies {
                let mut c = cookie::Cookie::new(c.name().to_string(), c.value().to_string());
                c.set_domain(ip.clone());
                let c = Cookie::try_from_raw_cookie(&c, &url.clone()).unwrap();
                cookie.insert(c, &url.clone()).unwrap();
            }
            self.api_url.vpn_param.url = url.to_string().trim_end_matches('/').to_string();
        }
        self.save_cookie();
        let req_start = Utc::now().timestamp_millis();
        let result = self.request::<String>(ApiName::PingVPN, None).await;
        let req_end = Utc::now().timestamp_millis();
        let latency = req_end - req_start;
        match result {
            Ok(resp) => match resp.code {
                0 => return latency,
                _ => {
                    log::warn!(
                        "failed to ping vpn with error {}: {}",
                        resp.code,
                        resp.message.unwrap()
                    );
                }
            },
            Err(err) => {
                log::warn!("failed to ping {}:{}: {}", ip, api_port, err);
            }
        }
        -1
    }

    async fn fetch_peer_info(&mut self, public_key: &String) -> Result<RespWgInfo, Error> {
        let mut otp = String::new();
        if let Some(code) = &self.conf.code {
            if !code.is_empty() {
                let code = utils::b32_decode(code);
                let offset = self.date_offset_sec / TIME_STEP as i32;
                let raw_otp = totp_offset(code.as_slice(), offset);
                otp = format!("{:06}", raw_otp.code);
                log::info!(
                    "2fa code generated: {}, {} seconds left",
                    &otp,
                    raw_otp.secs_left
                );
            }
        }
        if otp.is_empty() {
            log::info!("input your 2fa code:");
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

        log::info!(
            "found {} vpn(s), details: {:?}",
            vpn_info.len(),
            vpn_info
                .iter()
                .map(|i| i.en_name.clone())
                .collect::<Vec<String>>()
        );
        let filtered_vpn = vpn_info
            .into_iter()
            .filter(|vpn| {
                if let Some(server_name) = self.conf.vpn_server_name.clone() {
                    if vpn.en_name != server_name {
                        log::info!("skip {}, expect {}", vpn.en_name, server_name);
                        return false;
                    }
                }
                true
            })
            .filter(|vpn| {
                let mode = match vpn.protocol_mode {
                    1 => "tcp",
                    2 => "udp",
                    _ => "unknown protocol",
                };
                match mode {
                    "udp" => true,
                    "tcp" => true,
                    _ => {
                        log::info!(
                            "server name {} is not support {} wg for now",
                            vpn.en_name,
                            mode
                        );
                        false
                    }
                }
            })
            .collect();

        let vpn = match self.conf.vpn_select_strategy.clone() {
            Some(strategy) => match strategy.as_str() {
                STRATEGY_LATENCY => self.get_first_vpn_by_latency(filtered_vpn).await,
                STRATEGY_DEFAULT => self.get_first_available_vpn(filtered_vpn).await,
                _ => return Err(Error::Error("unsupported strategy".to_string())),
            },
            None => self.get_first_available_vpn(filtered_vpn).await,
        };

        let vpn = match vpn {
            Some(ref vpn) => vpn,
            None => return Err(Error::Error("no vpn available".to_string())),
        };
        let vpn_addr = format!("{}:{}", vpn.ip, vpn.vpn_port);
        log::info!("try connect to {}, address {}", vpn.en_name, vpn_addr);

        let key = self.conf.public_key.clone().unwrap();
        log::info!("try to get wg conf from remote");
        let wg_info = self.fetch_peer_info(&key).await?;
        let mtu = wg_info.setting.vpn_mtu;
        let dns = wg_info.setting.vpn_dns;
        let peer_key = wg_info.public_key;
        let public_key = self.conf.public_key.clone().unwrap();
        let private_key = self.conf.private_key.clone().unwrap();
        let address = format!("{}/{}", wg_info.ip, wg_info.ip_mask.parse::<u32>().unwrap());
        let address6 = (!wg_info.ipv6.is_empty())
            .then_some(format!("{}/128", wg_info.ipv6))
            .unwrap_or("".into());
        let route = [wg_info.setting.vpn_route_split, wg_info.setting.v6_route_split.unwrap_or_default()].concat();

        // corplink config
        let wg_conf = WgConf {
            address,
            address6,
            peer_address: vpn_addr,
            mtu,
            public_key,
            private_key,
            peer_key,
            route,
            dns,
            protocol: match vpn.protocol_mode {
                // tcp
                1 => 1,
                // udp
                _ => 0,
            },
        };
        Ok(wg_conf)
    }

    pub async fn keep_alive_vpn(&mut self, conf: &WgConf, interval: u64) {
        loop {
            log::info!("keep alive");
            match self.report_vpn_status(conf).await {
                Ok(_) => (),
                Err(err) => {
                    log::warn!("keep alive error: {}", err);
                    return;
                }
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    }

    pub async fn report_vpn_status(&mut self, conf: &WgConf) -> Result<(), Error> {
        let mut m = Map::new();
        m.insert("ip".to_string(), json!(conf.address));
        m.insert("public_key".to_string(), json!(conf.public_key));
        m.insert("mode".to_string(), json!("Split"));
        m.insert("type".to_string(), json!("100"));

        let resp = self
            .request::<Map<String, Value>>(ApiName::KeepAliveVPN, Some(m))
            .await?;
        match resp.code {
            0 => Ok(()),
            _ => Err(Error::Error(format!(
                "failed to report connection with error {}: {}",
                resp.code,
                resp.message.unwrap()
            ))),
        }
    }

    pub async fn disconnect_vpn(&mut self, wg_conf: &WgConf) -> Result<(), Error> {
        let mut m = Map::new();
        m.insert("ip".to_string(), json!(wg_conf.address));
        m.insert("public_key".to_string(), json!(wg_conf.public_key));
        m.insert("mode".to_string(), json!("Split"));
        m.insert("type".to_string(), json!("101"));
        let resp = self
            .request::<Map<String, Value>>(ApiName::DisconnectVPN, Some(m))
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
