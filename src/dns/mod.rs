use std::io::Error;

#[cfg(target_os = "macos")]
mod darwin;
#[cfg(target_os = "macos")]
pub use darwin::DNSManager;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::DNSManager;

#[cfg(target_os = "windows")]
mod win;
#[cfg(target_os = "windows")]
pub use win::DNSManager;

pub trait DNSManagerTrait {
    fn new() -> Self where Self: Sized;
    fn set_dns(&mut self, dns_servers: Vec<&str>, dns_search: Vec<&str>) -> Result<(), Error>;
    fn restore_dns(&self) -> Result<(), Error>;
} 