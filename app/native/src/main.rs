use anyhow::Result;
use ini::Ini;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr};
use boringtun::noise::{Tunn};
use boringtun::x25519::{StaticSecret, PublicKey};
use tokio::signal;

/// A simple Virtual Device that stores packets in a Vec
struct MyVirtualDevice {
    rx_queue: Vec<Vec<u8>>,
    tx_queue: Vec<Vec<u8>>,
}

impl Device for MyVirtualDevice {
    type RxToken<'a>
        = RxToken
    where
        Self: 'a;
    type TxToken<'a>
        = TxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.rx_queue.is_empty() {
            return None;
        }
        let buffer: Vec<u8> = self.rx_queue.remove(0);
        Some((RxToken(buffer), TxToken(self)))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken(self))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps: DeviceCapabilities = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ip; // We work at the IP layer, not Ethernet
        caps
    }
}

// Tokens required by smoltcp to manage buffers
struct RxToken(Vec<u8>);
impl smoltcp::phy::RxToken for RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.0)
    }
}

struct TxToken<'a>(&'a mut MyVirtualDevice);
impl<'a> smoltcp::phy::TxToken for TxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer: Vec<u8> = vec![0u8; len];
        let result: R = f(&mut buffer);
        self.0.tx_queue.push(buffer);
        result
    }
}

//#[tokio::main]
fn main() {
    println!("Initializing VPN from config...");
    let result: Result<String, anyhow::Error> =
        initialize_vpn_from_conf("/home/ethanm/Downloads/cachy-CA-1149.conf");
    match result {
        Ok(config_str) => println!("VPN Config Loaded:\n{}", config_str),
        Err(e) => eprintln!("Failed to initialize VPN: {}", e),
    }
}

fn initialize_vpn_from_conf(path: &str) -> Result<String> {
    // 1. Read and parse the file
    // let config_str: String = fs::read_to_string(path)?;
    let conf = Ini::load_from_file(path).unwrap();
    let config_str = conf
        .get_from(Some("Interface"), "Address")
        .unwrap()
        .to_string();

    let mut device = MyVirtualDevice {
        rx_queue: Vec::new(),
        tx_queue: Vec::new(),
    };

    let config = Config::new(HardwareAddress::Ip);

    let mut iface = Interface::new(config, &mut device, smoltcp::time::Instant::now());

    iface.update_ip_addrs(|addrs| {
        addrs
            .push(
                conf.get_from(Some("Interface"), "Address")
                    .unwrap()
                    .parse::<IpCidr>()
                    .unwrap(),
            )
            .unwrap();
    });

    let private_key:[u8; 32]= conf.get_from(Some("Interface"), "PrivateKey").unwrap().as_bytes().try_into().unwrap();
    let public_key:[u8; 32]= conf.get_from(Some("Peer"), "PublicKey").unwrap().as_bytes().try_into().unwrap();

    let tunnel = Tunn::new(StaticSecret::from(private_key), PublicKey::from(public_key), None, Some(25), 1, None);

    Ok(config_str)
}
