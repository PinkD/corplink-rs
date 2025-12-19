use std::io::{self, BufRead};

use anyhow::{anyhow, Context, Result};

use base32::Alphabet;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as base64;
use rand::rngs::OsRng;
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
