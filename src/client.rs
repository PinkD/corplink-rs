use chrono::Utc;
use std::collections::HashMap;
use std::fmt;
use std::path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use std::{fs, io};

use anyhow::{anyhow, bail, Context, Result};
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

pub async fn get_company_url(code: &str) -> anyhow::Result<RespCompany> {
    let c = ClientBuilder::new()
        // allow invalid certs because this cert is signed by corplink
        .danger_accept_invalid_certs(true)
        .build()
        .context("build client")?;
    let mut m = Map::new();
    m.insert("code".to_string(), json!(code));
    let body = serde_json::to_string(&m).context("serialize company request body")?;

    let resp = c
        .post(URL_GET_COMPANY)
        .body(body)
        .send()
        .await
        .context("get company")?
        .json::<Resp<RespCompany>>()
        .await
        .context("parse company resp")?;
    match resp.code {
        0 => resp.data.context("company response missing data"),
        _ => Err(anyhow!(resp
            .message
            .unwrap_or_else(|| "failed to fetch company info".to_string()))),
    }
}

impl Client {
    pub fn new(conf: Config) -> Result<Client> {
        let f = conf.conf_file.clone().context("config file path missing")?;
        let interface_name = conf
            .interface_name
            .clone()
            .context("interface name missing in config")?;
        let dir = match path::Path::new(&f).parent() {
            Some(dir) => dir,
            None => path::Path::new("."),
        };
        let cookie_file = dir.join(format!("{}_{}", interface_name, COOKIE_FILE_SUFFIX));
        log::info!("cookie file is: {}", cookie_file.to_string_lossy());

        let mut cookie_store = {
            let file = fs::File::open(&cookie_file).map(io::BufReader::new);
            match file {
                Ok(file) => CookieStore::load_json_all(file).or_else(|e| {
                    bail!(
                        "failed to load cookie store from {}: {e}",
                        cookie_file.display()
                    )
                })?,
                Err(_) => CookieStore::default(),
            }
        };
        let has_expired = cookie_store.iter_any().any(|cookie| cookie.is_expired());
        if has_expired {
            log::info!("some cookies are expired");
        }

        let mut headers = header::HeaderMap::new();

        if let Some(server) = conf.server.as_ref() {
            let server_url = Url::from_str(server.as_str())
                .with_context(|| format!("invalid server url: {server}"))?;

            if let Some(device_id) = conf.device_id.as_ref() {
                cookie_store
                    .insert_raw(&RawCookie::new("device_id", device_id), &server_url)
                    .context("failed to insert device_id cookie")?;
            }
            if let Some(device_name) = conf.device_name.as_ref() {
                cookie_store
                    .insert_raw(&RawCookie::new("device_name", device_name), &server_url)
                    .context("failed to insert device_name cookie")?;
            }

            if let Some(domain) = server_url.domain().or_else(|| server_url.host_str()) {
                if let Some(csrf_token) = cookie_store.get(domain, "/", "csrf-token") {
                    let value = header::HeaderValue::from_str(csrf_token.value())
                        .context("invalid csrf-token header value")?;
                    headers.insert("csrf-token", value);
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
            .build()
            .context("build http client")?;
        let conf_bak = conf.clone();
        Ok(Client {
            conf,
            cookie: Arc::clone(&cookie_store),
            c,
            api_url: ApiUrl::new(&conf_bak)?,
            date_offset_sec: 0,
        })
    }

    async fn change_state(&mut self, state: State) -> Result<()> {
        self.conf.state = Some(state);
        self.conf.save().await?;
        Ok(())
    }

    fn save_cookie(&self) -> Result<()> {
        let interface_name = self
            .conf
            .interface_name
            .as_ref()
            .context("interface name missing in config")?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(false)
            .open(format!("{}_{}", interface_name, COOKIE_FILE_SUFFIX))
            .map(io::BufWriter::new)
            .with_context(|| "failed to open cookie file for writing")?;
        let c = self
            .cookie
            .lock()
            .map_err(|e| anyhow!("failed to lock cookie store: {e}"))?;
        c.save_json(&mut file)
            .or_else(|e| bail!("failed to persist cookies to disk: {e}"))?;
        Ok(())
    }

    async fn request<T: DeserializeOwned + fmt::Debug>(
        &mut self,
        api: ApiName,
        body: Option<Map<String, Value>>,
    ) -> Result<Resp<T>> {
        let url = self.api_url.get_api_url(&api);

        let rb = match body {
            Some(body) => {
                let body = serde_json::to_string(&body)
                    .with_context(|| format!("failed to serialize request body for {api:?}"))?;
                self.c.post(url).body(body)
            }
            None => self.c.get(url),
        };

        let resp = rb
            .send()
            .await
            .with_context(|| format!("request {api:?} failed"))?;

        if !resp.status().is_success() {
            let msg = format!("logout because of bad resp code: {}", resp.status());
            self.handle_logout_err(msg).await?;
        }

        self.parse_time_offset_from_date_header(&resp);

        for (name, _) in resp.headers() {
            if name.as_str().eq_ignore_ascii_case("set-cookie") {
                log::info!("found set-cookie in header, saving cookie");
                self.save_cookie()?;
                break;
            }
        }
        let resp = resp
            .json::<Resp<T>>()
            .await
            .with_context(|| format!("failed to parse response for api {api:?}"))?;
        log::debug!("api {:#?} resp: {:#?}", api, resp);
        Ok(resp)
    }

    fn parse_time_offset_from_date_header(&mut self, resp: &Response) {
        let headers = resp.headers();
        if let Some(date) = headers.get("date") {
            match date.to_str() {
                Ok(date) => match httpdate::parse_http_date(date) {
                    Ok(date) => {
                        let now = SystemTime::now();
                        self.date_offset_sec = if now < date {
                            let date_offset = date
                                .duration_since(now)
                                .unwrap_or_else(|_| Duration::from_secs(0));
                            date_offset.as_secs().try_into().unwrap_or_default()
                        } else {
                            let date_offset = now
                                .duration_since(date)
                                .unwrap_or_else(|_| Duration::from_secs(0));
                            let offset: i32 = date_offset.as_secs().try_into().unwrap_or_default();
                            -offset
                        };
                    }
                    Err(e) => {
                        log::warn!("failed to parse date in header, ignore it: {}", e);
                    }
                },
                Err(e) => log::warn!("failed to read date header: {}", e),
            }
        }
    }

    pub fn need_login(&self) -> bool {
        matches!(self.conf.state.as_ref(), None | Some(State::Init))
    }

    async fn check_tps_token(&mut self, token: &String) -> Result<String> {
        // tps confirmed, try to login with token
        let mut m = Map::new();
        m.insert("token".to_string(), json!(token));

        let resp = self
            .request::<RespLogin>(ApiName::TpsTokenCheck, Some(m))
            .await?;
        match resp.code {
            0 => resp
                .data
                .context("tps token check missing redirect url")
                .map(|d| d.url),
            _ => {
                let msg = resp
                    .message
                    .unwrap_or_else(|| "tps token check failed".to_string());
                bail!(msg)
            }
        }
    }

    async fn get_otp_uri_from_tps(
        &mut self,
        method: &str,
        url: &String,
        token: &String,
    ) -> Result<String> {
        log::info!("old token is: {token}");
        log::info!("please scan the QR code or visit the following link to auth corplink:\n{url}");
        match TerminalQrCode::from_bytes(url.as_bytes()) {
            Ok(qr) => qr.print(),
            Err(e) => {log::warn!("failed to generate qr code: {e}");}
        }
        match method {
            PLATFORM_LARK | PLATFORM_OIDC => {
                log::info!("press enter if you finish auth");
                let stdin = io::stdin();
                stdin.lines().next();
                self.check_tps_token(token).await
            }
            _ => {
                // TODO: add all tps login support
                bail!("unsupported platform, please contact the developer");
            }
        }
    }

    async fn corplink_login(&mut self) -> Result<String> {
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
        bail!("failed to login with corplink")
    }

    async fn ldap_login(&mut self) -> Result<String> {
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
                    bail!("no password provided")
                };
            }
        }
        bail!("failed to login with ldap")
    }

    fn is_platform_or_default(&self, platform: &str) -> bool {
        if let Some(p) = &self.conf.platform {
            return p.is_empty() || platform == p;
        }
        true
    }

    async fn request_otp_code(&mut self) -> Result<String> {
        let m = Map::new();
        let resp = self.request::<RespOtp>(ApiName::OTP, Some(m)).await?;
        match resp.code {
            0 => Ok(resp.data.context("otp response missing data")?.url),
            _ => {
                let msg = resp
                    .message
                    .unwrap_or_else(|| "request otp code failed".to_string());
                bail!(msg)
            }
        }
    }

    async fn get_otp_uri_by_otp(
        &mut self,
        tps_login: &HashMap<String, RespTpsLoginMethod>,
        method: &String,
    ) -> Result<String> {
        let url = self.get_otp_uri(tps_login, method).await?;
        if url.is_empty() {
            self.request_otp_code().await
        } else {
            Ok(url)
        }
    }
    async fn get_otp_uri(
        &mut self,
        tps_login: &HashMap<String, RespTpsLoginMethod>,
        method: &String,
    ) -> Result<String> {
        if let Some(resp) = tps_login
            .get(method)
            .filter(|_| self.is_platform_or_default(method))
        {
            log::info!("try to login with third party platform {method}");
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
    pub async fn login(&mut self) -> Result<()> {
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
            let otp_uri = otp_uri?;
            if otp_uri.is_empty() {
                log::warn!("failed to login with method {method}");
                continue;
            }
            self.change_state(State::Login).await?;

            let url = Url::parse(&otp_uri).context("failed to parse otp uri")?;
            for (k, v) in url.query_pairs() {
                if k == "secret" {
                    log::info!("got 2fa token: {}", &v);
                    self.conf.code = Some(v.to_string());
                    self.conf.save().await?;
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
        bail!("no available login method, please provide a valid platform")
    }

    async fn get_login_method(&mut self) -> Result<RespLoginMethod> {
        let resp = self
            .request::<RespLoginMethod>(ApiName::LoginMethod, None)
            .await?;
        resp.data.context("login method response missing data")
    }

    // get 3rd party login methods and links, only lark(feishu) is tested
    async fn get_tps_login_method(&mut self) -> Result<Vec<RespTpsLoginMethod>> {
        let resp = self
            .request::<Vec<RespTpsLoginMethod>>(ApiName::TpsLoginMethod, None)
            .await?;
        Ok(resp.data.unwrap_or_default())
    }

    // get corplink login method, knowing result can be password or email
    async fn get_corplink_login_method(&mut self) -> Result<RespCorplinkLoginMethod> {
        let mut m = Map::new();
        m.insert("forget_password".to_string(), json!(false));
        m.insert("user_name".to_string(), json!(&self.conf.username));

        let resp = self
            .request::<RespCorplinkLoginMethod>(ApiName::CorplinkLoginMethod, Some(m))
            .await?;
        resp.data
            .context("corplink login method response missing data")
    }

    async fn login_with_password(&mut self, platform: &str) -> Result<String> {
        let mut password = self
            .conf
            .password
            .as_ref()
            .context("password is required for password login")?
            .clone();
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
                bail!("invalid platform {platform}")
            }
        }
        m.insert("password".to_string(), json!(password));
        m.insert("user_name".to_string(), json!(&self.conf.username));

        let resp = self
            .request::<RespLogin>(ApiName::LoginPassword, Some(m))
            .await?;
        match resp.code {
            0 => Ok(resp
                .data
                .context("password login response missing data")?
                .url),
            _ => {
                let msg = resp
                    .message
                    .unwrap_or_else(|| "login with password failed".to_string());
                bail!(msg)
            }
        }
    }

    async fn request_email_code(&mut self) -> Result<()> {
        let mut m = Map::new();
        m.insert("forget_password".to_string(), json!(false));
        m.insert("code_type".to_string(), json!("email"));
        m.insert("user_name".to_string(), json!(&self.conf.username));

        self.request::<Map<String, Value>>(ApiName::RequestEmailCode, Some(m))
            .await?;
        Ok(())
    }

    async fn login_with_email(&mut self) -> Result<String> {
        // tell server to send code to email
        log::info!("try to request code for email");
        self.request_email_code().await?;

        log::info!("input your code from email:");
        let input = utils::read_line().await?;
        let code = input.trim();
        let mut m = Map::new();
        m.insert("forget_password".to_string(), json!(false));
        m.insert("code_type".to_string(), json!("email"));
        m.insert("code".to_string(), json!(code));

        let resp = self
            .request::<RespLogin>(ApiName::LoginEmail, Some(m))
            .await?;
        match resp.code {
            0 => Ok(resp.data.context("email login response missing data")?.url),
            _ => bail!(format!(
                "failed to login with email code {}: {}",
                code,
                resp.message.unwrap_or_else(|| "unknown error".to_string())
            )),
        }
    }

    async fn handle_logout_err(&mut self, msg: String) -> Result<()> {
        self.change_state(State::Init)
            .await
            .context("failed to reset state after logout")?;
        bail!("operation failed because of logout: {msg}")
    }

    async fn list_vpn(&mut self) -> Result<Vec<RespVpnInfo>> {
        let resp = self
            .request::<Vec<RespVpnInfo>>(ApiName::ListVPN, None)
            .await?;
        match resp.code {
            0 => resp.data.context("list vpn response missing data"),
            101 => {
                let msg = resp
                    .message
                    .unwrap_or_else(|| "logout required".to_string());
                self.handle_logout_err(msg).await?;
                unreachable!()
            }
            _ => bail!(format!(
                "failed to list vpn with error {}: {}",
                resp.code,
                resp.message.unwrap_or_default()
            )),
        }
    }

    async fn get_first_vpn_by_latency(
        &mut self,
        vpn_info: Vec<RespVpnInfo>,
    ) -> Option<RespVpnInfo> {
        let mut fast_vpn = None;
        let mut min_latency = i64::MAX;
        for vpn in vpn_info {
            let latency = match self.ping_vpn(vpn.ip.clone(), vpn.api_port).await {
                Ok(latency) => latency,
                Err(err) => {
                    log::warn!("failed to ping {}:{}: {}", vpn.ip, vpn.api_port, err);
                    -1
                }
            };

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
            let latency = match self.ping_vpn(vpn.ip.clone(), vpn.api_port).await {
                Ok(latency) => latency,
                Err(err) => {
                    log::warn!("failed to ping {}:{}: {}", vpn.ip, vpn.api_port, err);
                    -1
                }
            };
            if latency != -1 {
                return Some(vpn);
            }
        }
        None
    }

    // ping vpn and return latency in ms. Will return Err on error
    async fn ping_vpn(&mut self, ip: String, api_port: u16) -> Result<i64> {
        {
            // config cookie
            let mut cookie = self
                .cookie
                .lock()
                .map_err(|e| anyhow!("failed to lock cookie store: {e}"))?;
            let server_url = self
                .conf
                .server
                .as_ref()
                .context("server url is required to ping vpn")?;

            let mut url = Url::from_str(server_url)
                .with_context(|| format!("invalid server url: {server_url}"))?;
            let mut cookies: Vec<Cookie> = Vec::new();
            for c in cookie.iter_any() {
                if c.domain.matches(&url.clone()) {
                    cookies.push(c.clone());
                }
            }
            url.set_host(Some(ip.as_str()))
                .context("failed to set ping host")?;
            url.set_port(Some(api_port))
                .or_else(|_| bail!("failed to set ping port"))?;
            for c in cookies {
                let mut c = cookie::Cookie::new(c.name().to_string(), c.value().to_string());
                c.set_domain(ip.clone());
                let c = Cookie::try_from_raw_cookie(&c, &url.clone())
                    .context("failed to convert raw cookie")?;
                cookie
                    .insert(c, &url.clone())
                    .context("failed to insert ping cookie")?;
            }
            self.api_url.vpn_param.url = url.to_string().trim_end_matches('/').to_string();
        }
        self.save_cookie()?;
        let req_start = Utc::now().timestamp_millis();
        let resp = self.request::<String>(ApiName::PingVPN, None).await?;
        let req_end = Utc::now().timestamp_millis();
        let latency = req_end - req_start;
        match resp.code {
            0 => Ok(latency),
            _ => bail!(format!(
                "failed to ping vpn with error {}: {}",
                resp.code,
                resp.message.unwrap_or_default()
            )),
        }
    }

    async fn fetch_peer_info(&mut self, public_key: &String) -> Result<RespWgInfo> {
        let mut otp = String::new();
        if let Some(code) = &self.conf.code {
            if !code.is_empty() {
                let code = utils::b32_decode(code)?;
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
            otp = utils::read_line().await?;
        }
        let mut m = Map::new();
        m.insert("public_key".to_string(), json!(public_key));
        m.insert("otp".to_string(), json!(otp));
        let resp = self
            .request::<RespWgInfo>(ApiName::ConnectVPN, Some(m))
            .await?;
        match resp.code {
            0 => resp.data.context("connect vpn response missing data"),
            101 => {
                let msg = resp
                    .message
                    .unwrap_or_else(|| "logout required".to_string());
                self.handle_logout_err(msg).await?;
                unreachable!()
            }
            _ => bail!(format!(
                "failed to fetch peer info with error {}: {}",
                resp.code,
                resp.message.unwrap_or_default()
            )),
        }
    }

    pub async fn connect_vpn(&mut self) -> Result<WgConf> {
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
                _ => bail!("unsupported strategy"),
            },
            None => self.get_first_available_vpn(filtered_vpn).await,
        };

        let vpn = match vpn {
            Some(ref vpn) => vpn,
            None => bail!("no vpn available"),
        };
        let vpn_addr = format!("{}:{}", vpn.ip, vpn.vpn_port);
        log::info!("try connect to {}, address {}", vpn.en_name, vpn_addr);

        let key = self
            .conf
            .public_key
            .as_ref()
            .context("public key missing in config")?
            .clone();
        log::info!("try to get wg conf from remote");
        let wg_info = self.fetch_peer_info(&key).await?;
        let mtu = wg_info.setting.vpn_mtu;
        let dns = wg_info.setting.vpn_dns;
        let peer_key = wg_info.public_key;
        let public_key = self
            .conf
            .public_key
            .as_ref()
            .context("public key missing in config")?
            .clone();
        let private_key = self
            .conf
            .private_key
            .as_ref()
            .context("private key missing in config")?
            .clone();
        let ip_mask = wg_info.ip_mask.parse::<u32>().context("invalid ip mask")?;
        let address = format!("{}/{}", wg_info.ip, ip_mask);
        let address6 = (!wg_info.ipv6.is_empty())
            .then_some(format!("{}/128", wg_info.ipv6))
            .unwrap_or("".into());
        let route = [
            wg_info.setting.vpn_route_split,
            wg_info.setting.v6_route_split.unwrap_or_default(),
        ]
        .concat();

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

    pub async fn report_vpn_status(&mut self, conf: &WgConf) -> Result<()> {
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
            _ => bail!(format!(
                "failed to report connection with error {}: {}",
                resp.code,
                resp.message.unwrap_or_default()
            )),
        }
    }

    pub async fn disconnect_vpn(&mut self, wg_conf: &WgConf) -> Result<()> {
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
            _ => bail!(format!(
                "failed to fetch peer info with error {}: {}",
                resp.code,
                resp.message.unwrap_or_default()
            )),
        }
    }
}
