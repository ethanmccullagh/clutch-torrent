use anyhow::Result;
use boringtun::noise::Tunn;
use boringtun::x25519::{PublicKey, StaticSecret, EphemeralSecret};
use ini::Ini;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr};
use base64::{Engine as _, engine::general_purpose};
use std::net::{SocketAddr, UdpSocket};
use std::any;
use tokio::signal;

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

pub fn setup_smoltcp_iface(conf: Ini) -> Result<Interface, anyhow::Error> {
    let config = Config::new(HardwareAddress::Ip);

    let mut device = MyVirtualDevice {
        rx_queue: Vec::new(),
        tx_queue: Vec::new(),
    };

    let vpn_ip: IpCidr = conf
        .get_from(Some("Interface"), "Address")
        .unwrap()
        .parse::<IpCidr>()
        .unwrap();

    let mut iface = Interface::new(config, &mut device, smoltcp::time::Instant::now());

    iface.update_ip_addrs(|addrs| {
        addrs.push(vpn_ip).unwrap();
    });

    Ok(iface)
}

pub fn setup_tunnel(conf: Ini) -> Result<Tunn, &'static str> {
    
    let private_b64 = conf.get_from(Some("Interface"), "PrivateKey").unwrap();
    let public_b64 = conf.get_from(Some("Peer"), "PublicKey").unwrap();

    let private_key = decode_key(private_b64).unwrap();
    let public_key = decode_key(public_b64).unwrap();

    let tunnel = Tunn::new(
        StaticSecret::from(private_key),
        PublicKey::from(public_key),
        None,
        Some(25),
        1,
        None,
    );
    tunnel
}

fn decode_key(b64: &str) -> Result<[u8; 32], String> {
    let bytes = general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("Base64 error: {}", e))?;
    bytes
        .try_into()
        .map_err(|_| "Key must be 32 bytes".to_string())
}
