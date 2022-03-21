use std::collections::HashMap;

use serde::Serialize;

use crate::config::Config;
use crate::template::Template;

const URL_GET_LOGIN_METHOD: &str = "https://{{url}}/api/lookup?os={{os}}&os_version={{version}}";
const URL_REQUEST_CODE: &str =
    "https://{{url}}/api/login/code/send?os={{os}}&os_version={{version}}";
const URL_VERIFY_CODE: &str =
    "https://{{url}}/api/login/code/verify?os={{os}}&os_version={{version}}";
const URL_LOGIN_PASSWORD: &str = "https://{{url}}/api/login?os={{os}}&os_version={{version}}";
const URL_LIST_VPN: &str = "https://{{url}}/api/vpn/list?os={{os}}&os_version={{version}}";

const URL_PING_VPN_HOST: &str = "https://{{ip}}:{{port}}/vpn/ping?os={{os}}&os_version={{version}}";
const URL_FETCH_PEER_INFO: &str =
    "https://{{ip}}:{{port}}/vpn/conn?os={{os}}&os_version={{version}}";
const URL_OPERATE_VPN: &str = "https://{{ip}}:{{port}}/vpn/report?os={{os}}&os_version={{version}}";

#[derive(Clone, Hash, Eq, PartialEq, Debug)]
pub enum ApiName {
    LoginMethod,
    RequestEmailCode,
    LoginPassword,
    LoginEmail,
    ListVPN,

    PingVPN,
    ConnectVPN,
    KeepAliveVPN,
    DisconnectVPN,
}

#[derive(Clone, Serialize)]
struct UserUrlParam {
    url: String,
    os: String,
    version: String,
}

#[derive(Clone, Serialize)]
pub struct VpnUrlParam {
    pub ip: String,
    pub port: u16,
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
    pub fn new(conf: &Config) -> ApiUrl {
        let os = "Android".to_string();
        let version = "2".to_string();
        let mut api_template = HashMap::new();

        api_template.insert(ApiName::LoginMethod, Template::new(URL_GET_LOGIN_METHOD));
        api_template.insert(ApiName::RequestEmailCode, Template::new(URL_REQUEST_CODE));
        api_template.insert(ApiName::LoginEmail, Template::new(URL_VERIFY_CODE));
        api_template.insert(ApiName::LoginPassword, Template::new(URL_LOGIN_PASSWORD));
        api_template.insert(ApiName::ListVPN, Template::new(URL_LIST_VPN));
        api_template.insert(ApiName::PingVPN, Template::new(URL_PING_VPN_HOST));
        api_template.insert(ApiName::ConnectVPN, Template::new(URL_FETCH_PEER_INFO));
        api_template.insert(ApiName::KeepAliveVPN, Template::new(URL_OPERATE_VPN));
        api_template.insert(ApiName::DisconnectVPN, Template::new(URL_OPERATE_VPN));

        let api_url = ApiUrl {
            user_param: UserUrlParam {
                url: conf.server.clone(),
                os: os.clone(),
                version: version.clone(),
            },
            vpn_param: VpnUrlParam {
                ip: "".to_string(),
                port: 0,
                os,
                version,
            },
            api_template,
        };

        api_url
    }

    pub fn get_api_url(&self, name: &ApiName) -> String {
        let user_param = &self.user_param;
        let vpn_param = &self.vpn_param;
        match name {
            ApiName::LoginMethod => self.api_template[name].render(user_param),
            ApiName::RequestEmailCode => self.api_template[name].render(user_param),
            ApiName::LoginEmail => self.api_template[name].render(user_param),
            ApiName::LoginPassword => self.api_template[name].render(user_param),
            ApiName::ListVPN => self.api_template[name].render(user_param),

            ApiName::PingVPN => self.api_template[name].render(vpn_param),
            ApiName::ConnectVPN => self.api_template[name].render(vpn_param),
            ApiName::KeepAliveVPN => self.api_template[name].render(vpn_param),
            ApiName::DisconnectVPN => self.api_template[name].render(vpn_param),
        }
    }
}
