use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use boringtun::noise::Tunn;
use boringtun::x25519::{PublicKey, StaticSecret};
use core::net::SocketAddr;
use ini::Ini;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::Tracer;
use smoltcp::wire::{HardwareAddress, IpCidr, Ipv4Address};
use tokio::sync::mpsc;

use crate::schema::*;

pub fn setup_engine<'a>(
    conf: Ini,
    device_receiver: mpsc::Receiver<Vec<u8>>,
    vpn_sender: mpsc::Sender<Vec<u8>>,
    local_sender: mpsc::Sender<Vec<u8>>,
) -> (Interface, Tracer<MyVirtualDevice>, SocketSet<'a>) {
    let config = Config::new(HardwareAddress::Ip);

    let device = MyVirtualDevice {
        rx_queue: device_receiver,
        vpn_tx: vpn_sender,
        local_tx: local_sender,
    };

    let mut tracer = Tracer::new(device, |_device, printer| {
        println!("---TRACER---");
        println!("{}", printer);
    });

    let vpn_ip: IpCidr = conf
        .get_from(Some("Interface"), "Address")
        .unwrap()
        .parse::<IpCidr>()
        .unwrap();

    let gateway_ip: Ipv4Address = conf
        .get_from(Some("Interface"), "DNS")
        .unwrap()
        .parse::<Ipv4Address>()
        .unwrap();

    let mut iface = Interface::new(config, &mut tracer, smoltcp::time::Instant::now());

    iface.update_ip_addrs(|addrs| {
        addrs.push(IpCidr::new(vpn_ip.address(), 32)).unwrap();
    });

    iface
        .routes_mut()
        .add_default_ipv4_route(gateway_ip)
        .unwrap();

    iface.set_any_ip(true);

    let sockets = SocketSet::new(vec![]);

    (iface, tracer, sockets)
}

pub fn setup_tunnel(conf: Ini) -> (Tunn, SocketAddr) {
    let private_b64 = conf.get_from(Some("Interface"), "PrivateKey").unwrap();
    let public_b64 = conf.get_from(Some("Peer"), "PublicKey").unwrap();
    let endpoint: SocketAddr = conf
        .get_from(Some("Peer"), "Endpoint")
        .unwrap()
        .parse()
        .unwrap();

    let private_key = decode_key(private_b64).unwrap();
    let public_key = decode_key(public_b64).unwrap();

    let tunnel = Tunn::new(
        StaticSecret::from(private_key),
        PublicKey::from(public_key),
        None,
        Some(25),
        1,
        None,
    )
    .unwrap();
    (tunnel, endpoint)
}

fn decode_key(b64: &str) -> Result<[u8; 32], String> {
    let bytes = general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("Base64 error: {}", e))?;
    bytes
        .try_into()
        .map_err(|_| "Key must be 32 bytes".to_string())
}
