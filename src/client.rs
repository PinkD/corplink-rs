use std::collections::HashMap;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use std::{fs, io};

use anyhow::{anyhow, bail, Context, Result};
use cookie::Cookie as RawCookie;
use cookie_store::{Cookie, CookieStore};
use futures::stream::{FuturesUnordered, StreamExt};
use reqwest::cookie::CookieStore as ReqwestCookieStore;
use reqwest::header;
use reqwest::{ClientBuilder, Response, Url};
use reqwest_cookie_store::CookieStoreMutex;
use serde::de::DeserializeOwned;
use serde_json::{json, Map, Value};
use sha2::Digest;

use crate::api::{ApiName, ApiUrl, CORPLINK_APP_VERSION, URL_GET_COMPANY};
use crate::config::{
    Config, WgConf, PLATFORM_CORPLINK, PLATFORM_CORPLINK_V1, PLATFORM_LARK, PLATFORM_LDAP,
    PLATFORM_OIDC, STRATEGY_DEFAULT, STRATEGY_LATENCY,
};
use crate::qrcode::TerminalQrCode;
use crate::resp::*;
use crate::state::State;
use crate::totp::{totp_offset, TIME_STEP};
use crate::utils;

const COOKIE_FILE_SUFFIX: &str = "cookies.json";

fn merge_additional_routes(
    mut routes: Vec<String>,
    additional_routes: &[String],
    has_ipv6_address: bool,
) -> Vec<String> {
    for route in additional_routes {
        if !crate::utils::is_valid_cidr(route) {
            log::warn!("ignoring invalid vpn_additional_routes CIDR: {:?}", route);
            continue;
        }
        if !has_ipv6_address && route.contains(':') {
            log::info!(
                "ignoring additional IPv6 route {:?} because the server did not assign an IPv6 address",
                route
            );
            continue;
        }
        if !routes.contains(route) {
            routes.push(route.clone());
        }
    }
    routes
}

async fn resolve_additional_domains(
    domains: &[String],
    has_ipv6_address: bool,
) -> Vec<String> {
    let mut routes = Vec::new();
    for configured_domain in domains {
        let domain = configured_domain.trim();
        if domain.is_empty() {
            log::warn!("ignoring empty vpn_additional_domains entry");
            continue;
        }

        match tokio::net::lookup_host((domain, 0)).await {
            Ok(addresses) => {
                let mut domain_routes = Vec::new();
                for address in addresses {
                    let ip = address.ip();
                    if ip.is_ipv6() && !has_ipv6_address {
                        continue;
                    }
                    let route = match ip {
                        std::net::IpAddr::V4(_) => format!("{ip}/32"),
                        std::net::IpAddr::V6(_) => format!("{ip}/128"),
                    };
                    if !domain_routes.contains(&route) {
                        domain_routes.push(route);
                    }
                }
                if domain_routes.is_empty() {
                    log::warn!(
                        "vpn_additional_domains entry {:?} returned no usable addresses",
                        domain
                    );
                } else {
                    log::info!(
                        "resolved additional VPN domain {:?} to {:?}",
                        domain,
                        domain_routes
                    );
                }
                for route in domain_routes {
                    if !routes.contains(&route) {
                        routes.push(route);
                    }
                }
            }
            Err(err) => {
                log::warn!(
                    "failed to resolve vpn_additional_domains entry {:?}: {}",
                    domain,
                    err
                );
            }
        }
    }
    routes
}

fn corplink_client_builder() -> ClientBuilder {
    ClientBuilder::new()
        // CorpLink deployments may use certificates signed by their own CA.
        .danger_accept_invalid_certs(true)
        // for debug
        // .proxy(reqwest::Proxy::all("socks5://192.168.111.233:8001").unwrap())
        .user_agent(format!(
            "CorpLink/{CORPLINK_APP_VERSION} (GooglePixel; Android 10; en)"
        ))
        .timeout(Duration::from_millis(10000))
}

#[derive(Clone)]
pub struct Client {
    conf: Config,
    cookie: Arc<CookieStoreMutex>,
    c: reqwest::Client,
    probe_client: reqwest::Client,
    api_url: ApiUrl,
    date_offset_sec: i32,
}

struct VpnProbeResponse {
    latency_ms: i64,
    set_cookie_headers: Vec<header::HeaderValue>,
}

struct SelectedVpn {
    vpn: RespVpnInfo,
    set_cookie_headers: Vec<header::HeaderValue>,
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

        // Keep probe responses out of the shared cookie store until an endpoint is selected.
        let probe_client = corplink_client_builder()
            .default_headers(headers.clone())
            .build()
            .context("build VPN probe HTTP client")?;
        let c = corplink_client_builder()
            .cookie_provider(Arc::clone(&cookie_store))
            .default_headers(headers)
            .build()
            .context("build http client")?;
        let conf_bak = conf.clone();
        Ok(Client {
            conf,
            cookie: Arc::clone(&cookie_store),
            c,
            probe_client,
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
                self.c
                    .post(url)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(body)
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
        let text = resp
            .text()
            .await
            .with_context(|| format!("failed to read response body for api {api:?}"))?;
        // Parse the envelope generically first. When the server-side session has
        // expired the server returns a non-zero code (e.g. 101) with a `data`
        // whose shape doesn't match T (ListVPN, for instance, gets an object where
        // it expects an array). Deserializing straight into Resp<T> would fail here
        // and bypass the code-based logout/retry handling, leaving a stale-session
        // run dead with a confusing parse error. So only coerce `data` into T once
        // we know code == 0; otherwise keep the code/message so callers can react.
        let raw: Resp<Value> = serde_json::from_str(&text).with_context(|| {
            format!("failed to parse response envelope for api {api:?}: {text}")
        })?;
        let data = match (raw.code, raw.data) {
            (0, Some(v)) => Some(
                serde_json::from_value::<T>(v)
                    .with_context(|| format!("failed to parse response data for api {api:?}"))?,
            ),
            _ => None,
        };
        let resp = Resp::<T> {
            code: raw.code,
            message: raw.message,
            data,
            action: raw.action,
        };
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
        let resp = self.request::<RespOtp>(ApiName::Otp, Some(m)).await?;
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

    // new feilian v1 login (/api/v1/login with AES-encrypted password).
    // opt-in via `"platform": "feilian_v1"`; the old login paths are untouched.
    async fn login_v1(&mut self) -> Result<()> {
        let password = self
            .conf
            .password
            .as_ref()
            .filter(|p| !p.is_empty())
            .context("platform feilian_v1 requires a password")?
            .clone();
        log::info!("try to login with platform feilian_v1");
        let enc = utils::feilian_v1_encrypt_password(&password);
        let mut m = Map::new();
        m.insert("login_scene".to_string(), json!(PLATFORM_CORPLINK));
        m.insert("account_type".to_string(), json!("userid"));
        m.insert("account".to_string(), json!(&self.conf.username));
        m.insert("password".to_string(), json!(enc));

        let resp = self
            .request::<RespLoginV1>(ApiName::LoginPasswordV1, Some(m))
            .await?;
        match resp.code {
            0 => {
                let data = resp.data.context("v1 login response missing data")?;
                if data.result != "success" {
                    bail!("v1 login returned unexpected result: {}", data.result);
                }
                log::info!("login success");
                self.change_state(State::Login).await?;

                // fetch the TOTP secret so 2fa codes can be generated locally,
                // mirroring the legacy login() flow. the v1 backend serves the
                // same /api/v2/p/otp endpoint and otpauth uri format.
                match self.request_otp_code().await {
                    Ok(otp_uri) if !otp_uri.is_empty() => {
                        let url = Url::parse(&otp_uri).context("failed to parse otp uri")?;
                        for (k, v) in url.query_pairs() {
                            if k == "secret" {
                                log::info!("got 2fa token: {}", &v);
                                self.conf.code = Some(v.to_string());
                                self.conf.save().await?;
                                break;
                            }
                        }
                    }
                    Ok(_) => {
                        log::info!(
                            "no otp code from server, will ask for 2fa code when connecting"
                        );
                    }
                    Err(e) => log::warn!("failed to get otp code: {e}"),
                }
                Ok(())
            }
            _ => {
                let msg = resp
                    .message
                    .unwrap_or_else(|| "v1 login failed".to_string());
                bail!(msg)
            }
        }
    }

    // choose right login method and login
    pub async fn login(&mut self) -> Result<()> {
        if self.conf.platform.as_deref() == Some(PLATFORM_CORPLINK_V1) {
            return self.login_v1().await;
        }
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
                log::info!("no otp code from server, will ask for 2fa code when connecting");
                self.change_state(State::Login).await?;
                return Ok(());
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

    async fn get_first_vpn_by_latency(&self, vpn_info: Vec<RespVpnInfo>) -> Option<SelectedVpn> {
        let mut fastest: Option<(i64, usize, SelectedVpn)> = None;

        let mut probes = vpn_info
            .into_iter()
            .enumerate()
            .map(|(index, vpn)| async move {
                let result = self.ping_vpn(&vpn.ip, vpn.api_port).await;
                (index, vpn, result)
            })
            .collect::<FuturesUnordered<_>>();

        while let Some((index, vpn, result)) = probes.next().await {
            match result {
                Ok(response) => {
                    log::info!(
                        "server name {}, latency {}ms",
                        vpn.en_name,
                        response.latency_ms
                    );
                    let should_replace = match &fastest {
                        Some((latency, best_index, _)) => {
                            (response.latency_ms, index) < (*latency, *best_index)
                        }
                        None => true,
                    };
                    if should_replace {
                        fastest = Some((
                            response.latency_ms,
                            index,
                            SelectedVpn {
                                vpn,
                                set_cookie_headers: response.set_cookie_headers,
                            },
                        ));
                    }
                }
                Err(err) => {
                    log::warn!("failed to ping {}:{}: {}", vpn.ip, vpn.api_port, err);
                }
            }
        }
        fastest.map(|(_, _, vpn)| vpn)
    }

    async fn get_first_available_vpn(&self, vpn_info: Vec<RespVpnInfo>) -> Option<SelectedVpn> {
        // Probes finish out of order, but the default strategy follows server-list priority.
        let mut results = std::iter::repeat_with(|| None)
            .take(vpn_info.len())
            .collect::<Vec<_>>();
        let mut next_index = 0;
        let mut probes = vpn_info
            .into_iter()
            .enumerate()
            .map(|(index, vpn)| async move {
                let result = self.ping_vpn(&vpn.ip, vpn.api_port).await;
                (index, vpn, result)
            })
            .collect::<FuturesUnordered<_>>();

        while let Some((index, vpn, result)) = probes.next().await {
            results[index] = Some((vpn, result));

            while next_index < results.len() {
                let Some((vpn, result)) = results[next_index].take() else {
                    break;
                };
                next_index += 1;

                match result {
                    Ok(response) => {
                        log::info!(
                            "server name {}, latency {}ms",
                            vpn.en_name,
                            response.latency_ms
                        );
                        return Some(SelectedVpn {
                            vpn,
                            set_cookie_headers: response.set_cookie_headers,
                        });
                    }
                    Err(err) => {
                        log::warn!("failed to ping {}:{}: {}", vpn.ip, vpn.api_port, err);
                    }
                }
            }
        }
        None
    }

    fn vpn_endpoint_url(&self, host: &str, api_port: u16) -> Result<Url> {
        let server_url = self
            .conf
            .server
            .as_ref()
            .context("server url is required to configure vpn endpoint")?;
        let server_url = Url::from_str(server_url)
            .with_context(|| format!("invalid server url: {server_url}"))?;
        let mut endpoint_url = Url::parse(&format!("{}://localhost", server_url.scheme()))
            .context("failed to construct vpn endpoint URL")?;
        match host.parse::<IpAddr>() {
            Ok(ip) => endpoint_url
                .set_ip_host(ip)
                .map_err(|_| anyhow!("failed to set vpn endpoint IP"))?,
            Err(_) => endpoint_url
                .set_host(Some(host))
                .context("failed to set vpn endpoint host")?,
        }
        endpoint_url
            .set_port(Some(api_port))
            .map_err(|_| anyhow!("failed to set vpn endpoint port"))?;
        Ok(endpoint_url)
    }

    fn probe_cookie_header(&self) -> Result<Option<header::HeaderValue>> {
        let server_url = self
            .conf
            .server
            .as_ref()
            .context("server url is required to prepare VPN probe cookies")?;
        let server_url = Url::from_str(server_url)
            .with_context(|| format!("invalid server url: {server_url}"))?;
        Ok(ReqwestCookieStore::cookies(
            self.cookie.as_ref(),
            &server_url,
        ))
    }

    fn prepare_vpn_endpoint(&mut self, ip: &str, api_port: u16) -> Result<Url> {
        let url = self.vpn_endpoint_url(ip, api_port)?;
        let mut cookie_store = self
            .cookie
            .lock()
            .map_err(|e| anyhow!("failed to lock cookie store: {e}"))?;
        let server_url = self
            .conf
            .server
            .as_ref()
            .context("server url is required to configure vpn endpoint")?;

        let server_url = Url::from_str(server_url)
            .with_context(|| format!("invalid server url: {server_url}"))?;
        let cookies: Vec<Cookie> = cookie_store
            .iter_any()
            .filter(|cookie| !cookie.is_expired() && cookie.domain.matches(&server_url))
            .cloned()
            .collect();
        for cookie in cookies {
            let raw_cookie =
                cookie::Cookie::new(cookie.name().to_string(), cookie.value().to_string());
            let endpoint_cookie = Cookie::try_from_raw_cookie(&raw_cookie, &url)
                .context("failed to convert raw cookie")?;
            cookie_store
                .insert(endpoint_cookie, &url)
                .context("failed to insert vpn endpoint cookie")?;
        }
        self.api_url.vpn_param.url = url.to_string().trim_end_matches('/').to_string();
        Ok(url)
    }

    // ping vpn and return latency in ms. Will return Err on error
    async fn ping_vpn(&self, ip: &str, api_port: u16) -> Result<VpnProbeResponse> {
        let endpoint_url = self.vpn_endpoint_url(ip, api_port)?;
        let mut api_url = self.api_url.clone();
        api_url.vpn_param.url = endpoint_url.to_string().trim_end_matches('/').to_string();

        let mut request = self
            .probe_client
            .get(api_url.get_api_url(&ApiName::PingVPN));
        if let Some(cookies) = self.probe_cookie_header()? {
            request = request.header(header::COOKIE, cookies);
        }

        let started = Instant::now();
        let response = request.send().await.context("VPN probe request failed")?;
        let status = response.status();
        let set_cookie_headers = response
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .cloned()
            .collect();
        let body = response
            .text()
            .await
            .context("failed to read VPN probe response body")?;
        let latency_ms = started.elapsed().as_millis().min(i64::MAX as u128) as i64;

        if !status.is_success() {
            bail!("VPN probe returned HTTP status {status}");
        }
        let resp: Resp<Value> = serde_json::from_str(&body)
            .with_context(|| format!("failed to parse VPN probe response: {body}"))?;
        match resp.code {
            0 => Ok(VpnProbeResponse {
                latency_ms,
                set_cookie_headers,
            }),
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
            let is_tps_login = matches!(
                self.conf.platform.as_deref(),
                Some(PLATFORM_LARK | PLATFORM_OIDC)
            );
            if is_tps_login {
                log::info!("use empty 2fa code (tps login already verified)");
            } else {
                log::info!("input your 2fa code:");
                otp = utils::read_line().await?;
            }
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

        let selected_vpn = vpn.context("no vpn available")?;
        let vpn = &selected_vpn.vpn;
        let endpoint_url = self.prepare_vpn_endpoint(&vpn.ip, vpn.api_port)?;
        // Persist only cookies returned by the selected endpoint probe.
        ReqwestCookieStore::set_cookies(
            self.cookie.as_ref(),
            &mut selected_vpn.set_cookie_headers.iter(),
            &endpoint_url,
        );
        self.save_cookie()?;
        let vpn_addr = match vpn.ip.parse::<IpAddr>() {
            Ok(ip) => SocketAddr::new(ip, vpn.vpn_port).to_string(),
            Err(_) => format!("{}:{}", vpn.ip, vpn.vpn_port),
        };
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
        let has_ipv6_address = !wg_info.ipv6.is_empty();
        let address6 = has_ipv6_address
            .then_some(format!("{}/128", wg_info.ipv6))
            .unwrap_or_default();
        let mut allowed_ips = match self.conf.route_mode.clone().unwrap_or_default() {
            crate::config::RouteMode::Split => {
                log::info!("route_mode = split");
                let mut routes = wg_info.setting.vpn_route_split;
                let v6 = wg_info.setting.v6_route_split.unwrap_or_default();
                if has_ipv6_address {
                    routes.extend(v6);
                } else if !v6.is_empty() {
                    log::info!(
                        "ignoring {} IPv6 split routes because the server did not assign an IPv6 address",
                        v6.len()
                    );
                }
                routes
            }
            crate::config::RouteMode::Full => {
                log::info!("route_mode = full");
                let v4 = wg_info.setting.vpn_route_full;
                let v6 = wg_info.setting.v6_route_full.unwrap_or_default();
                log::info!(
                    "route_mode=full, server returned vpn_route_full ({} entries): {:?}",
                    v4.len(),
                    v4
                );
                log::info!(
                    "route_mode=full, server returned v6_route_full ({} entries): {:?}",
                    v6.len(),
                    v6
                );
                let mut routes = v4;
                if has_ipv6_address {
                    routes.extend(v6);
                } else if !v6.is_empty() {
                    log::info!(
                        "ignoring {} IPv6 full-tunnel routes because the server did not assign an IPv6 address",
                        v6.len()
                    );
                }
                if routes.is_empty() {
                    bail!(
                        "route_mode=full but server returned no usable routes; \
                         refuse to fall back to 0.0.0.0/0 to avoid peer-IP routing loop that blocks all traffic"
                    );
                }
                routes
            }
        };

        let mut additional_routes = self
            .conf
            .vpn_additional_routes
            .clone()
            .unwrap_or_default();
        if let Some(domains) = self.conf.vpn_additional_domains.as_deref() {
            additional_routes
                .extend(resolve_additional_domains(domains, has_ipv6_address).await);
        }
        if !additional_routes.is_empty() {
            let before = allowed_ips.len();
            allowed_ips = merge_additional_routes(
                allowed_ips,
                &additional_routes,
                has_ipv6_address,
            );
            log::info!(
                "additional VPN routes merged: {} -> {} entries",
                before,
                allowed_ips.len()
            );
        }

        // Restrict server and user-added routes to the optional whitelist, then
        // carve out the optional denylist. A configured empty whitelist
        // intentionally yields no AllowedIPs/routes; invalid entries fail closed.
        if let Some(allowed) = self.conf.vpn_allowed_routes.as_deref() {
            for route in allowed {
                if !crate::utils::is_valid_cidr(route) {
                    log::warn!("ignoring invalid vpn_allowed_routes CIDR: {:?}", route);
                }
            }
        }
        let before = allowed_ips.len();
        allowed_ips = crate::utils::apply_route_filters(
            &allowed_ips,
            self.conf.vpn_allowed_routes.as_deref(),
            self.conf.vpn_disallowed_routes.as_deref(),
        );
        if self.conf.vpn_allowed_routes.is_some() || self.conf.vpn_disallowed_routes.is_some() {
            log::info!(
                "VPN route filters applied: {} -> {} entries",
                before,
                allowed_ips.len()
            );
        }

        // Auto-carve the VPN peer endpoint IP out of allowed_ips. In full-tunnel mode
        // the server typically returns 0.0.0.0/0, which would match the outer UDP
        // packets going to the peer itself, producing a routing loop (black hole).
        // Mirrors wg-quick's behavior of excluding the endpoint from routes. No-op
        // when the peer IP isn't covered by any allowed_ip (e.g. split mode).
        match vpn.ip.parse::<std::net::IpAddr>() {
            Ok(peer_ip) => {
                let peer_cidr = match peer_ip {
                    std::net::IpAddr::V4(_) => format!("{}/32", peer_ip),
                    std::net::IpAddr::V6(_) => format!("{}/128", peer_ip),
                };
                let before = allowed_ips.len();
                let mut carved = Vec::with_capacity(allowed_ips.len());
                for a in &allowed_ips {
                    carved.extend(crate::utils::subtract_cidr_from_cidr(a, &peer_cidr));
                }
                if carved.len() != before {
                    log::info!(
                        "auto-carved peer endpoint {} out of allowed_ips: {} -> {} entries",
                        peer_cidr,
                        before,
                        carved.len()
                    );
                }
                allowed_ips = carved;
            }
            Err(e) => {
                log::warn!(
                    "could not parse vpn.ip {:?} as IP, skipping peer-IP carve-out: {}",
                    vpn.ip,
                    e
                );
            }
        }
        log::info!(
            "final allowed_ips ({} entries): {:?}",
            allowed_ips.len(),
            allowed_ips
        );
        let auto_setup_routes = self.conf.auto_setup_routes.unwrap_or(true);
        let routes = if auto_setup_routes {
            allowed_ips.clone()
        } else {
            log::info!("auto_setup_routes is disabled, skip setting routes");
            Vec::new()
        };

        // corplink config
        let wg_conf = WgConf {
            address,
            address6,
            peer_address: vpn_addr,
            mtu,
            public_key,
            private_key,
            peer_key,
            allowed_ips,
            routes,
            dns,
            // `force_protocol`, when set, overrides the server-advertised `protocol_mode`
            protocol: match self.conf.force_protocol.as_deref() {
                Some(p) if p.eq_ignore_ascii_case("udp") => 0,
                Some(p) if p.eq_ignore_ascii_case("tcp") => 1,
                _ => match vpn.protocol_mode {
                    // tcp
                    1 => 1,
                    // udp
                    _ => 0,
                },
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
        m.insert(
            "mode".to_string(),
            json!(match self.conf.route_mode.clone().unwrap_or_default() {
                crate::config::RouteMode::Split => "Split",
                crate::config::RouteMode::Full => "Full",
            }),
        );
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
        m.insert(
            "mode".to_string(),
            json!(match self.conf.route_mode.clone().unwrap_or_default() {
                crate::config::RouteMode::Split => "Split",
                crate::config::RouteMode::Full => "Full",
            }),
        );
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

    // log out the current terminal, freeing its server-side session/terminal
    // quota (servers cap concurrent terminals, e.g. nankai allows only 3).
    // best-effort: callers treat failures as non-fatal since we're exiting.
    pub async fn logout(&mut self) -> Result<()> {
        let url = self.api_url.get_api_url(&ApiName::Logout);
        let mut req = self.c.get(url);
        // /api/logout validates a csrf-token header (double-submit against the
        // cookie). the token is only known after login, so read it from the
        // cookie store here rather than relying on the default headers.
        if let Some(server) = self.conf.server.as_ref() {
            if let Ok(server_url) = Url::parse(server) {
                if let Some(domain) = server_url.domain().or_else(|| server_url.host_str()) {
                    let token = {
                        let store = self
                            .cookie
                            .lock()
                            .map_err(|e| anyhow!("failed to lock cookie store: {e}"))?;
                        store
                            .get(domain, "/", "csrf-token")
                            .map(|c| c.value().to_string())
                    };
                    if let Some(token) = token {
                        if let Ok(value) = header::HeaderValue::from_str(&token) {
                            req = req.header("csrf-token", value);
                        }
                    }
                }
            }
        }
        // the endpoint replies with a 302 redirect (not JSON), so just confirm
        // the request went through instead of parsing a response body.
        let resp = req.send().await.context("logout request failed")?;
        log::info!("logout (current terminal) status: {}", resp.status());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::{oneshot, Barrier};
    use tokio::time::{sleep, timeout};

    use super::{merge_additional_routes, resolve_additional_domains, Client, ReqwestCookieStore};
    use crate::config::Config;
    use crate::resp::RespVpnInfo;
    use crate::utils::apply_route_filters;

    async fn start_probe_server(
        barrier: Arc<Barrier>,
        response_delay: Duration,
        session: &'static str,
    ) -> (u16, oneshot::Receiver<String>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (request_tx, request_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            loop {
                let count = stream.read(&mut buffer).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            request_tx
                .send(String::from_utf8(request).unwrap())
                .unwrap();

            barrier.wait().await;
            sleep(response_delay).await;
            let body = r#"{"code":0}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nSet-Cookie: vpn_session={session}; Path=/\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).await.unwrap();
        });
        (port, request_rx, task)
    }

    fn vpn_info(port: u16, name: &str) -> RespVpnInfo {
        RespVpnInfo {
            api_port: port,
            vpn_port: port,
            ip: "127.0.0.1".to_string(),
            protocol_mode: 2,
            name: name.to_string(),
            en_name: name.to_string(),
            icon: String::new(),
            id: 0,
            timeout: 0,
        }
    }

    fn test_client() -> Client {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let mut conf: Config = serde_json::from_value(json!({
            "company_name": "test",
            "username": "test",
            "server": "http://127.0.0.1",
            "interface_name": format!("corplink-probe-test-{unique}"),
            "device_id": "test-device"
        }))
        .unwrap();
        conf.conf_file = Some(
            std::env::temp_dir()
                .join(format!("corplink-probe-test-{unique}.json"))
                .to_string_lossy()
                .into_owned(),
        );
        Client::new(conf).unwrap()
    }

    #[tokio::test]
    async fn concurrent_default_probe_preserves_order_and_isolates_cookie_state() {
        let barrier = Arc::new(Barrier::new(3));
        let (first_port, first_request, first_task) =
            start_probe_server(Arc::clone(&barrier), Duration::from_millis(75), "first").await;
        let (second_port, second_request, second_task) =
            start_probe_server(Arc::clone(&barrier), Duration::ZERO, "second").await;

        let client = test_client();
        let candidates = vec![
            vpn_info(first_port, "first"),
            vpn_info(second_port, "second"),
        ];

        let selected = timeout(Duration::from_secs(5), async {
            let (selected, _) =
                tokio::join!(client.get_first_available_vpn(candidates), barrier.wait());
            selected
        })
        .await
        .expect("VPN probes did not run concurrently")
        .expect("no VPN was selected");

        assert_eq!(selected.vpn.en_name, "first");
        assert!(selected.set_cookie_headers[0]
            .to_str()
            .unwrap()
            .starts_with("vpn_session=first"));
        let first_request = first_request.await.unwrap().to_ascii_lowercase();
        let second_request = second_request.await.unwrap().to_ascii_lowercase();
        assert!(first_request.contains("cookie: device_id=test-device"));
        assert!(second_request.contains("cookie: device_id=test-device"));
        assert!(first_request.contains("user-agent: corplink/201000 "));
        assert!(second_request.contains("user-agent: corplink/201000 "));

        {
            let cookie_store = client.cookie.lock().unwrap();
            assert!(cookie_store.get("127.0.0.1", "/", "vpn_session").is_none());
        }
        let endpoint_url = client
            .vpn_endpoint_url(&selected.vpn.ip, selected.vpn.api_port)
            .unwrap();
        ReqwestCookieStore::set_cookies(
            client.cookie.as_ref(),
            &mut selected.set_cookie_headers.iter(),
            &endpoint_url,
        );
        {
            let cookie_store = client.cookie.lock().unwrap();
            assert_eq!(
                cookie_store
                    .get("127.0.0.1", "/", "vpn_session")
                    .unwrap()
                    .value(),
                "first"
            );
        }

        first_task.await.unwrap();
        second_task.await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_latency_probe_selects_the_fastest_endpoint() {
        let barrier = Arc::new(Barrier::new(3));
        let (slow_port, slow_request, slow_task) =
            start_probe_server(Arc::clone(&barrier), Duration::from_millis(75), "slow").await;
        let (fast_port, fast_request, fast_task) =
            start_probe_server(Arc::clone(&barrier), Duration::ZERO, "fast").await;
        let client = test_client();
        let candidates = vec![vpn_info(slow_port, "slow"), vpn_info(fast_port, "fast")];

        let selected = timeout(Duration::from_secs(5), async {
            let (selected, _) =
                tokio::join!(client.get_first_vpn_by_latency(candidates), barrier.wait());
            selected
        })
        .await
        .expect("VPN probes did not run concurrently")
        .expect("no VPN was selected");

        assert_eq!(selected.vpn.en_name, "fast");
        assert!(selected.set_cookie_headers[0]
            .to_str()
            .unwrap()
            .starts_with("vpn_session=fast"));
        slow_request.await.unwrap();
        fast_request.await.unwrap();
        slow_task.await.unwrap();
        fast_task.await.unwrap();
    }

    #[test]
    fn vpn_endpoint_urls_use_server_scheme_and_candidate_host() {
        let mut client = test_client();
        client.conf.server = Some("https://127.0.0.1/base?source=config#fragment".to_string());

        let hostname_endpoint = client
            .vpn_endpoint_url("vpn-node.example.com", 8443)
            .unwrap();
        let ipv4_endpoint = client.vpn_endpoint_url("192.0.2.1", 8443).unwrap();
        let ipv6_endpoint = client.prepare_vpn_endpoint("2001:db8::1", 8443).unwrap();
        let ipv6_cookies = ReqwestCookieStore::cookies(client.cookie.as_ref(), &ipv6_endpoint)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        assert_eq!(
            hostname_endpoint.as_str(),
            "https://vpn-node.example.com:8443/"
        );
        assert_eq!(ipv4_endpoint.as_str(), "https://192.0.2.1:8443/");
        assert_eq!(ipv6_endpoint.as_str(), "https://[2001:db8::1]:8443/");
        assert!(ipv6_cookies.contains("device_id=test-device"));
    }

    #[test]
    fn additional_routes_are_validated_deduplicated_and_merged() {
        let routes = merge_additional_routes(
            vec!["10.0.0.0/8".to_string()],
            &[
                "10.0.0.0/8".to_string(),
                "20.205.243.160/28".to_string(),
                "invalid".to_string(),
                "2001:db8::/32".to_string(),
            ],
            false,
        );

        assert_eq!(routes, vec!["10.0.0.0/8", "20.205.243.160/28"]);
    }

    #[test]
    fn additional_ipv6_routes_are_kept_with_an_ipv6_address() {
        let routes = merge_additional_routes(Vec::new(), &["2001:db8::/32".to_string()], true);

        assert_eq!(routes, vec!["2001:db8::/32"]);
    }

    #[test]
    fn additional_routes_are_merged_before_route_filters() {
        let routes = merge_additional_routes(
            vec!["10.0.0.0/8".to_string()],
            &["20.205.243.160/28".to_string()],
            false,
        );
        let allowed = ["20.205.243.160/28".to_string()];

        assert_eq!(
            apply_route_filters(&routes, Some(&allowed), None),
            vec!["20.205.243.160/28"]
        );
    }

    #[tokio::test]
    async fn additional_domains_are_resolved_to_host_routes() {
        let routes = resolve_additional_domains(&["127.0.0.1".to_string()], false).await;

        assert_eq!(routes, vec!["127.0.0.1/32"]);
    }
}
