use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use boringtun::noise::Tunn;
use boringtun::x25519::{PublicKey, StaticSecret};
use core::net::SocketAddr;
use ini::Ini;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::wire::{HardwareAddress, IpCidr, Ipv4Address};
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::schema::*;

pub fn setup_engine(
    conf: Ini
) -> (VpnEngine <'static>, mpsc::Sender<ProxyMessage>, mpsc::Receiver<Vec<u8>>, mpsc::Receiver<Vec<u8>>, mpsc::Sender<Vec<u8>>){
    let (device_vpn_sender, device_vpn_receiver) = mpsc::channel::<Vec<u8>>(100);
    let (device_local_sender, device_local_receiver) = mpsc::channel::<Vec<u8>>(100);
    let (device_rx_sender, device_rx_receiver) = mpsc::channel::<Vec<u8>>(100);
    let (control_sender, control_receiver) = mpsc::channel::<ProxyMessage>(100);

    let (iface, device) =
        setup_smoltcp_iface(conf.clone(), device_rx_receiver, device_vpn_sender, device_local_sender).unwrap();

    let sockets = SocketSet::new(vec![]);
    
    let connection_map = HashMap::<u64, (SocketHandle, mpsc::Sender<Vec<u8>>)>::new();

    (VpnEngine {
        iface: iface,
        sockets: sockets,
        device: device,
        control_receiver: control_receiver,
        connection_map: connection_map
    }, control_sender, device_vpn_receiver, device_local_receiver, device_rx_sender)
}
pub fn setup_smoltcp_iface(
    conf: Ini,
    rx_queue: mpsc::Receiver<Vec<u8>>,
    vpn_queue: mpsc::Sender<Vec<u8>>,
    local_queue: mpsc::Sender<Vec<u8>>,
) -> Result<(Interface, MyVirtualDevice)> {
    let config = Config::new(HardwareAddress::Ip);

    let mut device = MyVirtualDevice {
        rx_queue: rx_queue,
        vpn_tx: vpn_queue,
        local_tx: local_queue
    };

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

    let mut iface = Interface::new(config, &mut device, smoltcp::time::Instant::now());

    iface.update_ip_addrs(|addrs| {
        addrs.push(vpn_ip).unwrap();
    });

    iface.routes_mut().add_default_ipv4_route(gateway_ip).unwrap();

    Ok((iface, device))
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
    ).unwrap();
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
