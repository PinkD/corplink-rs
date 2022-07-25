#[derive(serde::Deserialize)]
pub struct Resp<T> {
    pub code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct RespCompany {
    pub name: String,
    pub zh_name: String,
    pub en_name: String,
    pub domain: String,
    pub enable_self_signed: bool,
    pub self_signed_cert: String,
    pub enable_public_key: bool,
    pub public_key: String,
}

#[derive(serde::Deserialize)]
pub struct RespLoginMethod {
    pub mfa: bool,
    pub auth: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct RespLogin {
    pub url: String,
}

#[derive(serde::Deserialize)]
pub struct RespVpnInfo {
    pub api_port: u16,
    pub vpn_port: u16,
    pub ip: String,
    // 1 for tcp, 2 for udp, we only support udp for now
    pub protocol_mode: i32,
    // useless
    pub name: String,
    pub en_name: String,
    pub icon: String,
    pub id: i32,
    pub timeout: i32,
}

#[derive(serde::Deserialize)]
pub struct RespWgExtraInfo {
    pub vpn_mtu: u32,
    pub vpn_dns: String,
    pub vpn_dns_backup: String,
    pub vpn_dns_domain_split: Vec<String>,
    pub vpn_route_full: Vec<String>,
    pub vpn_route_split: Vec<String>,
}

#[derive(serde::Deserialize)]
pub struct RespWgInfo {
    pub ip: String,
    pub ipv6: String,
    pub ip_mask: String,
    pub public_key: String,
    pub protocol_version: Option<String>,
    pub setting: RespWgExtraInfo,
    pub mode: u32,
}
