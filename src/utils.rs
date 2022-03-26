use std::error::Error;
use std::io::{self, BufRead};

use base32::Alphabet;
use rand::rngs::OsRng;
use x25519_dalek::{PublicKey, StaticSecret};

pub async fn read_line() -> String {
    io::stdin().lock().lines().next().unwrap().unwrap()
}

pub fn b32_decode(s: &str) -> Vec<u8> {
    return base32::decode(Alphabet::RFC4648 { padding: true }, s).unwrap();
}

pub fn service_name(name: &String) -> String {
    format!("wg-quick@{}.service", name)
}

pub fn gen_wg_keypair() -> (String, String) {
    let csprng = OsRng {};
    let sk = StaticSecret::new(csprng);
    let pk = PublicKey::from(&sk);
    (base64::encode(pk.to_bytes()), base64::encode(sk.to_bytes()))
}

pub fn gen_public_key_from_private(private_key: &String) -> Result<String, Box<dyn Error>> {
    match base64::decode(private_key) {
        Ok(key) => {
            let key: [u8; 32] = key.try_into().unwrap();
            let sk = StaticSecret::from(key);
            let public_key = PublicKey::from(&sk);
            Ok(base64::encode(public_key.to_bytes()))
        }
        Err(e) => Err(format!("failed to base64 decode {}: {}", private_key, e).into()),
    }
}
