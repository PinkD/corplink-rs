use std::io::{self, BufRead};

use base32::Alphabet;

pub async fn read_line() -> String {
    io::stdin().lock().lines().next().unwrap().unwrap()
}

pub fn b32_decode(s: &str) -> Vec<u8> {
    return base32::decode(Alphabet::RFC4648 { padding: true }, s).unwrap();
}

pub fn service_name(name: &String) -> String {
    format!("wg-quick@{}.service", name)
}