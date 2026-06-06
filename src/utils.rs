use std::io::{self, BufRead};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use anyhow::{anyhow, Context, Result};

use aes::Aes256;
use base32::Alphabet;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as base64;
use cbc::cipher::{block_padding::Pkcs7, generic_array::GenericArray, BlockEncryptMut, KeyIvInit};
use rand::rngs::OsRng;
use sha1::{Digest, Sha1};
use x25519_dalek::{PublicKey, StaticSecret};

pub async fn read_line() -> Result<String> {
    io::stdin()
        .lock()
        .lines()
        .next()
        .context("stdin closed")?
        .context("failed to read line")
}

pub fn b32_decode(s: &str) -> Result<Vec<u8>> {
    base32::decode(Alphabet::RFC4648 { padding: true }, s)
        .context("failed to decode base32")
}

pub fn gen_wg_keypair() -> (String, String) {
    let csprng = OsRng {};
    let sk = StaticSecret::random_from_rng(csprng);
    let pk = PublicKey::from(&sk);
    (base64.encode(pk.to_bytes()), base64.encode(sk.to_bytes()))
}

pub fn gen_public_key_from_private(private_key: &String) -> Result<String> {
    let key = base64
        .decode(private_key)
        .with_context(|| format!("failed to base64 decode private key {private_key}"))?;
    let key: [u8; 32] = key
        .try_into()
        .map_err(|_| anyhow!("private key has invalid length"))?;
    let sk = StaticSecret::from(key);
    let public_key = PublicKey::from(&sk);
    Ok(base64.encode(public_key.to_bytes()))
}

pub fn b64_decode_to_hex(s: &str) -> Result<String> {
    let data = base64
        .decode(s)
        .with_context(|| format!("failed to base64 decode string {s}"))?;
    let mut hex = String::new();
    for c in data {
        hex.push_str(format!("{c:02x}").as_str());
    }
    Ok(hex)
}

// Encrypt a password the way the official feilian client (v1 login, `/api/v1/login`)
// does, reverse-engineered from `wireguard.Wireguard.encryptByAesCbc(generateFixedString(), pwd)`
// in libgojni.so. Both key and IV are derived from fixed constants, so the output is
// deterministic and acts as a stable password hash on the server side.
//
//   KEY = hex(md5("9007199254740991"))   -> 32 ascii bytes (AES-256 key)
//   IV  = hex(sha1(KEY))[..16]            -> 16 ascii bytes
//   out = lower_hex( AES-256-CBC(KEY, IV, PKCS7(password)) )
pub fn feilian_v1_encrypt_password(password: &str) -> String {
    let key = format!("{:x}", md5::compute(b"9007199254740991"));
    let iv = hex::encode(Sha1::digest(key.as_bytes()));
    let iv = &iv[..16];

    let ct = cbc::Encryptor::<Aes256>::new(
        GenericArray::from_slice(key.as_bytes()),
        GenericArray::from_slice(iv.as_bytes()),
    )
    .encrypt_padded_vec_mut::<Pkcs7>(password.as_bytes());
    hex::encode(ct)
}

/// Returns a list of CIDR strings covering all addresses in `outer` except those in `inner`.
///
/// - Disjoint (no overlap) → `[outer]` unchanged.
/// - `inner` fully covers `outer` → `[]` (everything is removed).
/// - `outer` strictly contains `inner` → a minimal set of CIDRs whose union is
///   exactly `outer \ inner` (one CIDR per bit of prefix difference).
/// - Address-family mismatch or unparseable input → `[outer]` unchanged
///   (conservative: never silently drop routes).
///
/// Used by the VPN client to carve user-specified ranges
/// (`vpn_disallowed_routes`, e.g. local LAN) out of the server-supplied AllowedIPs
/// (e.g. `0.0.0.0/0` in full-tunnel mode).
pub fn subtract_cidr_from_cidr(outer: &str, inner: &str) -> Vec<String> {
    let Some((outer_base, outer_prefix)) = parse_cidr(outer) else {
        return vec![outer.to_string()];
    };
    let Some((inner_base, inner_prefix)) = parse_cidr(inner) else {
        return vec![outer.to_string()];
    };
    if !same_family(outer_base, inner_base) {
        return vec![outer.to_string()];
    }
    // inner covers outer (inner prefix is shorter or equal and contains outer's base)
    if inner_prefix <= outer_prefix
        && cidr_contains_ip(inner_base, inner_prefix, outer_base)
    {
        return Vec::new();
    }
    // outer doesn't contain inner → disjoint
    if !cidr_contains_ip(outer_base, outer_prefix, inner_base) {
        return vec![outer.to_string()];
    }
    // outer strictly contains inner, carve by recursive bisection
    let host_bits = host_bit_count(outer_base);
    carve_recursive(outer_base, outer_prefix, host_bits, inner_base, inner_prefix)
}

fn parse_cidr(cidr: &str) -> Option<(IpAddr, u8)> {
    let (ip_s, prefix_s) = cidr.split_once('/')?;
    let ip: IpAddr = ip_s.parse().ok()?;
    let prefix: u8 = prefix_s.parse().ok()?;
    if prefix > host_bit_count(ip) {
        return None;
    }
    Some((canonicalize(ip, prefix), prefix))
}

fn host_bit_count(ip: IpAddr) -> u8 {
    match ip {
        IpAddr::V4(_) => 32,
        IpAddr::V6(_) => 128,
    }
}

fn same_family(a: IpAddr, b: IpAddr) -> bool {
    matches!(
        (a, b),
        (IpAddr::V4(_), IpAddr::V4(_)) | (IpAddr::V6(_), IpAddr::V6(_))
    )
}

fn mask_v4(prefix: u8) -> u32 {
    if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) }
}

fn mask_v6(prefix: u8) -> u128 {
    if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) }
}

fn canonicalize(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(a) => IpAddr::V4(Ipv4Addr::from(u32::from(a) & mask_v4(prefix))),
        IpAddr::V6(a) => IpAddr::V6(Ipv6Addr::from(u128::from(a) & mask_v6(prefix))),
    }
}

fn cidr_contains_ip(base: IpAddr, prefix: u8, ip: IpAddr) -> bool {
    match (base, ip) {
        (IpAddr::V4(b), IpAddr::V4(a)) => {
            let m = mask_v4(prefix);
            (u32::from(b) & m) == (u32::from(a) & m)
        }
        (IpAddr::V6(b), IpAddr::V6(a)) => {
            let m = mask_v6(prefix);
            (u128::from(b) & m) == (u128::from(a) & m)
        }
        _ => false,
    }
}

fn split_half(base: IpAddr, prefix: u8) -> (IpAddr, IpAddr) {
    match base {
        IpAddr::V4(b) => {
            let bu = u32::from(b);
            let bit = 1u32 << (31 - prefix);
            (IpAddr::V4(Ipv4Addr::from(bu)), IpAddr::V4(Ipv4Addr::from(bu | bit)))
        }
        IpAddr::V6(b) => {
            let bu = u128::from(b);
            let bit = 1u128 << (127 - prefix);
            (IpAddr::V6(Ipv6Addr::from(bu)), IpAddr::V6(Ipv6Addr::from(bu | bit)))
        }
    }
}

fn carve_recursive(
    outer_base: IpAddr,
    outer_prefix: u8,
    host_bits: u8,
    inner_base: IpAddr,
    inner_prefix: u8,
) -> Vec<String> {
    if outer_prefix == inner_prefix {
        // caller guaranteed containment ⇒ outer == inner
        return Vec::new();
    }
    debug_assert!(outer_prefix < inner_prefix && outer_prefix < host_bits);
    let new_prefix = outer_prefix + 1;
    let (lower, upper) = split_half(outer_base, outer_prefix);
    let (containing, sibling) = if cidr_contains_ip(lower, new_prefix, inner_base) {
        (lower, upper)
    } else {
        (upper, lower)
    };
    let mut out = Vec::with_capacity((inner_prefix - outer_prefix) as usize);
    out.push(format!("{}/{}", sibling, new_prefix));
    out.extend(carve_recursive(
        containing,
        new_prefix,
        host_bits,
        inner_base,
        inner_prefix,
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    fn cidr_contains_addr(cidr: &str, addr: IpAddr) -> bool {
        let (base_s, prefix_s) = cidr.split_once('/').expect("bad CIDR in test helper");
        let base: IpAddr = base_s.parse().expect("bad base in test helper");
        let prefix: u8 = prefix_s.parse().expect("bad prefix in test helper");
        match (base, addr) {
            (IpAddr::V4(b), IpAddr::V4(a)) => {
                let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
                (u32::from(b) & mask) == (u32::from(a) & mask)
            }
            (IpAddr::V6(b), IpAddr::V6(a)) => {
                let mask = if prefix == 0 { 0 } else { u128::MAX << (128 - prefix) };
                (u128::from(b) & mask) == (u128::from(a) & mask)
            }
            _ => false,
        }
    }

    #[test]
    fn subtract_disjoint_returns_outer_unchanged() {
        let out = subtract_cidr_from_cidr("10.0.0.0/24", "192.168.0.0/16");
        assert_eq!(out, vec!["10.0.0.0/24"]);
    }

    #[test]
    fn subtract_inner_equal_to_outer_returns_empty() {
        let out = subtract_cidr_from_cidr("10.0.0.0/24", "10.0.0.0/24");
        assert!(out.is_empty());
    }

    #[test]
    fn subtract_inner_covers_outer_returns_empty() {
        // inner /16 swallows outer /24
        let out = subtract_cidr_from_cidr("10.0.5.0/24", "10.0.0.0/16");
        assert!(out.is_empty());
    }

    #[test]
    fn subtract_outer_covers_inner_produces_expected_complement() {
        // 10.0.0.0/16 minus 10.0.5.0/24 = 8 CIDRs (one per split level from /16 to /24).
        let out = sorted(subtract_cidr_from_cidr("10.0.0.0/16", "10.0.5.0/24"));
        let expected = sorted(vec![
            "10.0.0.0/22".into(),
            "10.0.4.0/24".into(),
            "10.0.6.0/23".into(),
            "10.0.8.0/21".into(),
            "10.0.16.0/20".into(),
            "10.0.32.0/19".into(),
            "10.0.64.0/18".into(),
            "10.0.128.0/17".into(),
        ]);
        assert_eq!(out, expected);
    }

    #[test]
    fn subtract_family_mismatch_returns_outer_unchanged() {
        let out = subtract_cidr_from_cidr("10.0.0.0/24", "::/0");
        assert_eq!(out, vec!["10.0.0.0/24"]);
        let out = subtract_cidr_from_cidr("2001:db8::/32", "10.0.0.0/8");
        assert_eq!(out, vec!["2001:db8::/32"]);
    }

    #[test]
    fn subtract_unparseable_returns_outer_unchanged() {
        assert_eq!(
            subtract_cidr_from_cidr("not-a-cidr", "10.0.0.0/8"),
            vec!["not-a-cidr"]
        );
        assert_eq!(
            subtract_cidr_from_cidr("10.0.0.0/8", "not-a-cidr"),
            vec!["10.0.0.0/8"]
        );
        assert_eq!(
            subtract_cidr_from_cidr("10.0.0.0/99", "10.0.0.0/24"),
            vec!["10.0.0.0/99"]
        );
    }

    #[test]
    fn subtract_single_host_from_default_route_yields_32_cidrs_not_covering_host() {
        // /32 subtraction is a special case of general CIDR subtraction.
        let out = subtract_cidr_from_cidr("0.0.0.0/0", "1.2.3.4/32");
        assert_eq!(out.len(), 32);
        let host: IpAddr = "1.2.3.4".parse().unwrap();
        for cidr in &out {
            assert!(!cidr_contains_addr(cidr, host), "{} should not cover 1.2.3.4", cidr);
        }
    }

    #[test]
    fn subtract_lan_cidr_from_default_route_yields_16_cidrs_not_covering_any_lan_ip() {
        // The motivating scenario: 0.0.0.0/0 - 10.68.0.0/16 in full-tunnel mode.
        // Going /0 down to /16 is 16 split levels, so 16 complement CIDRs.
        let out = subtract_cidr_from_cidr("0.0.0.0/0", "10.68.0.0/16");
        assert_eq!(out.len(), 16);
        // Sample IPs that must NOT be covered by any complement CIDR:
        for ip_s in ["10.68.0.1", "10.68.40.10", "10.68.255.255"] {
            let ip: IpAddr = ip_s.parse().unwrap();
            for cidr in &out {
                assert!(
                    !cidr_contains_addr(cidr, ip),
                    "complement CIDR {} should not cover LAN IP {}",
                    cidr,
                    ip_s
                );
            }
        }
        // And an IP outside 10.68.0.0/16 must be covered by exactly one complement CIDR:
        let outside: IpAddr = "8.8.8.8".parse().unwrap();
        let covers = out
            .iter()
            .filter(|c| cidr_contains_addr(c, outside))
            .count();
        assert_eq!(covers, 1);
    }

    #[test]
    fn subtract_ipv6_default_minus_host_yields_128_cidrs() {
        let out = subtract_cidr_from_cidr("::/0", "2001:db8::1/128");
        assert_eq!(out.len(), 128);
    }

    #[test]
    fn subtract_non_canonical_inner_is_normalized() {
        // 10.0.5.7/24 is not canonical; mask-aligned form is 10.0.5.0/24.
        // Subtraction should treat it as the canonical form, not as a literal.
        let out = sorted(subtract_cidr_from_cidr("10.0.0.0/16", "10.0.5.7/24"));
        let expected = sorted(subtract_cidr_from_cidr("10.0.0.0/16", "10.0.5.0/24"));
        assert_eq!(out, expected);
    }
}
