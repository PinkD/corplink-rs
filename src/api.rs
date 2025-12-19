use std::collections::HashMap;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::Config;
use crate::template::Template;

pub const URL_GET_COMPANY: &str = "https://corplink.volcengine.cn/api/match";

const URL_GET_LOGIN_METHOD: &str = "{{url}}/api/login/setting?os={{os}}&os_version={{version}}";
const URL_GET_TPS_LOGIN_METHOD: &str = "{{url}}/api/tpslogin/link?os={{os}}&os_version={{version}}";
const URL_GET_TPS_TOKEN_CHECK: &str =
    "{{url}}/api/tpslogin/token/check?os={{os}}&os_version={{version}}";
const URL_GET_CORPLINK_LOGIN_METHOD: &str = "{{url}}/api/lookup?os={{os}}&os_version={{version}}";
const URL_REQUEST_CODE: &str = "{{url}}/api/login/code/send?os={{os}}&os_version={{version}}";
const URL_VERIFY_CODE: &str = "{{url}}/api/login/code/verify?os={{os}}&os_version={{version}}";
const URL_LOGIN_PASSWORD: &str = "{{url}}/api/login?os={{os}}&os_version={{version}}";
const URL_LIST_VPN: &str = "{{url}}/api/vpn/list?os={{os}}&os_version={{version}}";

const URL_PING_VPN_HOST: &str = "{{url}}/vpn/ping?os={{os}}&os_version={{version}}";
const URL_FETCH_PEER_INFO: &str = "{{url}}/vpn/conn?os={{os}}&os_version={{version}}";
const URL_OPERATE_VPN: &str = "{{url}}/vpn/report?os={{os}}&os_version={{version}}";
const URL_OTP: &str = "{{url}}/api/v2/p/otp?os={{os}}&os_version={{version}}";

#[derive(Clone, Hash, Eq, PartialEq, Debug)]
pub enum ApiName {
    LoginMethod,
    TpsLoginMethod,
    TpsTokenCheck,
    CorplinkLoginMethod,
    RequestEmailCode,
    LoginPassword,
    LoginEmail,
    ListVPN,

    PingVPN,
    ConnectVPN,
    KeepAliveVPN,
    DisconnectVPN,
    OTP,
}

#[derive(Clone, Serialize)]
struct UserUrlParam {
    url: String,
    os: String,
    version: String,
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
    pub vpn_param: VpnUrlParam,
    api_template: HashMap<ApiName, Template>,
}

impl ApiUrl {
    pub fn new(conf: &Config) -> Result<ApiUrl> {
        let os = "Android".to_string();
        let version = "2".to_string();
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
        api_template.insert(ApiName::ListVPN, Template::new(URL_LIST_VPN));
        api_template.insert(ApiName::PingVPN, Template::new(URL_PING_VPN_HOST));
        api_template.insert(ApiName::ConnectVPN, Template::new(URL_FETCH_PEER_INFO));
        api_template.insert(ApiName::KeepAliveVPN, Template::new(URL_OPERATE_VPN));
        api_template.insert(ApiName::DisconnectVPN, Template::new(URL_OPERATE_VPN));
        api_template.insert(ApiName::OTP, Template::new(URL_OTP));

        Ok(ApiUrl {
            user_param: UserUrlParam {
                url: conf
                    .server
                    .clone()
                    .context("server url missing in config")?,
                os: os.clone(),
                version: version.clone(),
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
            ApiName::ListVPN => self.api_template[name].render(user_param),
            ApiName::OTP => self.api_template[name].render(user_param),

            ApiName::PingVPN => self.api_template[name].render(vpn_param),
            ApiName::ConnectVPN => self.api_template[name].render(vpn_param),
            ApiName::KeepAliveVPN => self.api_template[name].render(vpn_param),
            ApiName::DisconnectVPN => self.api_template[name].render(vpn_param),
        }
    }
}
