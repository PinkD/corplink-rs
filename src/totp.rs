// code from basic-otp 0.1.1
use byteorder::{BigEndian, ReadBytesExt, WriteBytesExt};
use hmacsha1::{hmac_sha1, SHA1_DIGEST_BYTES};
use std::io::Cursor;
use std::time;

pub fn hotp(key: &[u8], counter: u64, digits: u32) -> u32 {
    let mut counter_bytes = vec![];
    counter_bytes.write_u64::<BigEndian>(counter).unwrap();

    let hmac = hmac_sha1(key, &counter_bytes);

    let dyn_offset = (hmac[SHA1_DIGEST_BYTES - 1] & 0xf) as usize;
    let dyn_range = &hmac[dyn_offset..dyn_offset + 4];

    let mut rdr = Cursor::new(dyn_range);
    let s_num = rdr.read_u32::<BigEndian>().unwrap() & 0x7fffffff;

    s_num % 10u32.pow(digits)
}

const DIGITS: u32 = 6;
pub const TIME_STEP: u64 = 30;

#[derive(Debug)]
pub struct TotpSlot {
    pub code: u32,
    pub secs_left: u32,
}

pub fn totp_offset(key: &[u8], slot_offset: i32) -> TotpSlot {
    let now = time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .expect("Current time is before unix epoch");
    let slot = (now.as_secs() / TIME_STEP) as i64 + slot_offset as i64;

    let code = hotp(key, slot as u64, DIGITS);
    let secs_left = (TIME_STEP - now.as_secs() % TIME_STEP) as u32;
    TotpSlot { code, secs_left }
}

#[allow(dead_code)]
pub fn totp(key: &[u8]) -> u32 {
    let now = time::SystemTime::now()
        .duration_since(time::UNIX_EPOCH)
        .expect("Current time is before unix epoch");
    let slot = now.as_secs() / TIME_STEP;

    hotp(key, slot, DIGITS)
}
