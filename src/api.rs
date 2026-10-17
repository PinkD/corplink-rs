use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::Config;
use crate::template::Template;

pub const URL_GET_COMPANY: &str = "https://corplink.volcengine.cn/api/match";
pub(crate) const CORPLINK_APP_VERSION: &str = "3.3.17";
pub(crate) const CORPLINK_BUILD_NUMBER: &str = "8135";

const URL_GET_LOGIN_METHOD: &str = "{{url}}/api/login/setting?os={{os}}&os_version={{version}}";
const URL_GET_TPS_LOGIN_METHOD: &str = "{{url}}/api/tpslogin/link?os={{os}}&os_version={{version}}";
const URL_GET_TPS_TOKEN_CHECK: &str =
    "{{url}}/api/tpslogin/token/check?os={{os}}&os_version={{version}}";
const URL_GET_CORPLINK_LOGIN_METHOD: &str = "{{url}}/api/lookup?os={{os}}&os_version={{version}}";
const URL_REQUEST_CODE: &str = "{{url}}/api/login/code/send?os={{os}}&os_version={{version}}";
const URL_VERIFY_CODE: &str = "{{url}}/api/login/code/verify?os={{os}}&os_version={{version}}";
const URL_LOGIN_PASSWORD: &str = "{{url}}/api/login?os={{os}}&os_version={{version}}";
const URL_LOGIN_PASSWORD_V1: &str =
    "{{url}}/api/v1/login?os={{os}}&os_version={{version}}&client_source=FeiLian";
const URL_LIST_VPN: &str = "{{url}}/api/vpn/list?app_version={{app_version}}&brand={{brand}}&build_number={{build_number}}&client_source={{client_source}}&language={{language}}&model={{model}}&os={{os}}&os_release={{os_release}}&os_version={{version}}&soc={{soc}}&timestamp={{timestamp}}";

const URL_PING_VPN_HOST: &str = "{{url}}/vpn/ping?os={{os}}&os_version={{version}}";
const URL_FETCH_PEER_INFO: &str = "{{url}}/vpn/conn?app_version={{app_version}}&brand={{brand}}&build_number={{build_number}}&client_source={{client_source}}&language={{language}}&model={{model}}&os={{os}}&os_release={{os_release}}&os_version={{version}}&soc={{soc}}&timestamp={{timestamp}}";
const URL_OPERATE_VPN: &str = "{{url}}/vpn/report?os={{os}}&os_version={{version}}";
const URL_OTP: &str = "{{url}}/api/v2/p/otp?os={{os}}&os_version={{version}}";
// log out the current terminal so it frees the server-side session/terminal
// quota. logout_all=false only signs out this device. responds with a 302.
const URL_LOGOUT: &str = "{{url}}/api/logout?os={{os}}&os_version={{version}}&logout_all=false";

#[derive(Clone, Hash, Eq, PartialEq, Debug)]
pub enum ApiName {
    LoginMethod,
    TpsLoginMethod,
    TpsTokenCheck,
    CorplinkLoginMethod,
    RequestEmailCode,
    LoginPassword,
    LoginPasswordV1,
    LoginEmail,
    ListVPN,

    PingVPN,
    ConnectVPN,
    KeepAliveVPN,
    DisconnectVPN,
    Otp,
    Logout,
}

#[derive(Clone, Serialize)]
struct UserUrlParam {
    url: String,
    os: String,
    version: String,
    app_version: String,
}

#[derive(Clone, Serialize)]
struct ListVpnUrlParam {
    url: String,
    app_version: String,
    brand: String,
    build_number: String,
    client_source: String,
    language: String,
    model: String,
    os: String,
    os_release: String,
    version: String,
    soc: String,
    timestamp: String,
}

#[derive(Clone, Serialize)]
pub struct VpnUrlParam {
    pub url: String,
    os: String,
    version: String,
}

#[derive(Clone)]
pub struct ApiUrl {
    user_param: UserUrlParam,
    list_vpn_param: ListVpnUrlParam,
    pub vpn_param: VpnUrlParam,
    api_template: HashMap<ApiName, Template>,
}

fn unix_timestamp_seconds() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
        .to_string()
}

fn linux_os_release() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(release) = std::fs::read_to_string("/etc/os-release") {
            for line in release.lines() {
                if let Some(value) = line.strip_prefix("ID=") {
                    return value.trim_matches('"').to_string();
                }
            }
        }
        "linux".to_string()
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::consts::OS.to_string()
    }
}

fn linux_os_version() -> String {
    #[cfg(target_os = "linux")]
    {
        if let Ok(release) = std::fs::read_to_string("/etc/os-release") {
            for line in release.lines() {
                if let Some(value) = line.strip_prefix("PRETTY_NAME=") {
                    return value.trim_matches('"').to_string();
                }
            }
        }
        "Linux".to_string()
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::env::consts::OS.to_string()
    }
}

fn cpu_soc() -> String {
    match std::env::consts::ARCH {
        "aarch64" => "aarch64".to_string(),
        "x86_64" => "x86_64".to_string(),
        arch => arch.to_string(),
    }
}

impl ApiUrl {
    pub fn new(conf: &Config) -> Result<ApiUrl> {
        let os = "Android".to_string();
        let version = "2".to_string();
        let server_url = conf
            .server
            .clone()
            .context("server url missing in config")?;
        let mut api_template = HashMap::new();

        api_template.insert(ApiName::LoginMethod, Template::new(URL_GET_LOGIN_METHOD));
        api_template.insert(
            ApiName::TpsLoginMethod,
            Template::new(URL_GET_TPS_LOGIN_METHOD),
        );
        api_template.insert(
            ApiName::TpsTokenCheck,
            Template::new(URL_GET_TPS_TOKEN_CHECK),
        );
        api_template.insert(
            ApiName::CorplinkLoginMethod,
            Template::new(URL_GET_CORPLINK_LOGIN_METHOD),
        );
        api_template.insert(ApiName::RequestEmailCode, Template::new(URL_REQUEST_CODE));
        api_template.insert(ApiName::LoginEmail, Template::new(URL_VERIFY_CODE));
        api_template.insert(ApiName::LoginPassword, Template::new(URL_LOGIN_PASSWORD));
        api_template.insert(
            ApiName::LoginPasswordV1,
            Template::new(URL_LOGIN_PASSWORD_V1),
        );
        api_template.insert(ApiName::ListVPN, Template::new(URL_LIST_VPN));
        api_template.insert(ApiName::PingVPN, Template::new(URL_PING_VPN_HOST));
        api_template.insert(ApiName::ConnectVPN, Template::new(URL_FETCH_PEER_INFO));
        api_template.insert(ApiName::KeepAliveVPN, Template::new(URL_OPERATE_VPN));
        api_template.insert(ApiName::DisconnectVPN, Template::new(URL_OPERATE_VPN));
        api_template.insert(ApiName::Otp, Template::new(URL_OTP));
        api_template.insert(ApiName::Logout, Template::new(URL_LOGOUT));

        Ok(ApiUrl {
            user_param: UserUrlParam {
                url: server_url.clone(),
                os: os.clone(),
                version: version.clone(),
                app_version: CORPLINK_APP_VERSION.to_string(),
            },
            list_vpn_param: ListVpnUrlParam {
                url: server_url,
                app_version: CORPLINK_APP_VERSION.to_string(),
                brand: "".to_string(),
                build_number: CORPLINK_BUILD_NUMBER.to_string(),
                client_source: "FeiLian".to_string(),
                language: "en".to_string(),
                model: "".to_string(),
                os: "Linux".to_string(),
                os_release: linux_os_release(),
                version: linux_os_version(),
                soc: cpu_soc(),
                timestamp: unix_timestamp_seconds(),
            },
            vpn_param: VpnUrlParam {
                url: "".to_string(),
                os,
                version,
            },
            api_template,
        })
    }

    pub fn get_api_url(&self, name: &ApiName) -> String {
        let user_param = &self.user_param;
        let vpn_param = &self.vpn_param;
        match name {
            ApiName::LoginMethod => self.api_template[name].render(user_param),
            ApiName::TpsLoginMethod => self.api_template[name].render(user_param),
            ApiName::TpsTokenCheck => self.api_template[name].render(user_param),
            ApiName::CorplinkLoginMethod => self.api_template[name].render(user_param),
            ApiName::RequestEmailCode => self.api_template[name].render(user_param),
            ApiName::LoginEmail => self.api_template[name].render(user_param),
            ApiName::LoginPassword => self.api_template[name].render(user_param),
            ApiName::LoginPasswordV1 => self.api_template[name].render(user_param),
            ApiName::ListVPN => {
                let mut list_vpn_param = self.list_vpn_param.clone();
                list_vpn_param.timestamp = unix_timestamp_seconds();
                self.api_template[name].render(&list_vpn_param)
            }
            ApiName::Otp => self.api_template[name].render(user_param),
            ApiName::Logout => self.api_template[name].render(user_param),

            ApiName::PingVPN => self.api_template[name].render(vpn_param),
            ApiName::ConnectVPN => {
                let mut param = self.list_vpn_param.clone();
                param.url = self.vpn_param.url.clone();
                param.timestamp = unix_timestamp_seconds();
                self.api_template[name].render(&param)
            }
            ApiName::KeepAliveVPN => self.api_template[name].render(vpn_param),
            ApiName::DisconnectVPN => self.api_template[name].render(vpn_param),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn list_vpn_url_matches_official_linux_shape() {
        let conf: Config = serde_json::from_value(json!({
            "company_name": "test",
            "username": "test",
            "server": "https://vpn.example.com"
        }))
        .unwrap();

        let api_url = ApiUrl::new(&conf).unwrap();
        let url = api_url.get_api_url(&ApiName::ListVPN);

        assert!(url.starts_with("https://vpn.example.com/api/vpn/list?"));
        assert!(url.contains("app_version=3.3.17"));
        assert!(url.contains("build_number=8135"));
        assert!(url.contains("client_source=FeiLian"));
        assert!(url.contains("os=Linux"));
        assert!(url.contains("timestamp="));
    }
}
