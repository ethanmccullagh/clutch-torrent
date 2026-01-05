use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use boringtun::noise::Tunn;
use boringtun::x25519::{PublicKey, StaticSecret};
use ini::Ini;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::wire::{HardwareAddress, IpCidr};
use std::collections::VecDeque;
use tokio::net::UdpSocket;
use core::net::SocketAddr;
use std::collections::HashMap;

use crate::schema::*;

pub async fn setup_engine(conf: Ini) -> VpnEngine{
    let mut tunnel = setup_tunnel(conf.clone()).unwrap();

    let (mut iface, mut device) = setup_smoltcp_iface(conf.clone()).unwrap();

    let sockets = SocketSet::new(vec![]); 

    let udp_socket = UdpSocket::bind("0.0.0.0:0").await.unwrap();

    let connection_map = HashMap::<usize, SocketHandle>::new();

    let endpoint: SocketAddr = conf
        .get_from(Some("Peer"), "Endpoint")
        .unwrap()
        .parse()
        .unwrap();

    VpnEngine {
        tunn : tunnel,
        iface : iface,
        sockets : sockets,
        device: device,
        connection_map : connection_map,
        udp_socket: udp_socket,
        endpoint: endpoint
    }


}
pub fn setup_smoltcp_iface(conf: Ini) -> Result<(Interface, MyVirtualDevice), anyhow::Error> {
    let config = Config::new(HardwareAddress::Ip);

    let mut device = MyVirtualDevice {
        rx_queue: VecDeque::new(),
        tx_queue: VecDeque::new(),
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

    Ok((iface, device))
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
