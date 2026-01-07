use anyhow::Result;
use base64::{engine::general_purpose, Engine as _};
use boringtun::noise::Tunn;
use boringtun::x25519::{PublicKey, StaticSecret};
use core::net::SocketAddr;
use ini::Ini;
use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::wire::{HardwareAddress, IpCidr};
use std::collections::HashMap;
use tokio::sync::mpsc;

use crate::schema::*;

pub async fn setup_engine(
    conf: Ini,
    udp_send_channel: mpsc::Sender<ChannelMessage>,
    udp_recv_channel: mpsc::Receiver<ChannelMessage>,
) -> (VpnEngine <'static>, mpsc::Sender<ProxyMessage>){
    let tunnel = setup_tunnel(conf.clone()).unwrap();
    let (device_txqueue_s, device_txqueue_r) = mpsc::channel::<Vec<u8>>(100);
    let (device_rxqueue_s, device_rxqueue_r) = mpsc::channel::<Vec<u8>>(100);
    let (control_sender, control_receiver) = mpsc::channel::<ProxyMessage>(100);

    let (iface, device) =
        setup_smoltcp_iface(conf.clone(), device_rxqueue_r, device_txqueue_s).unwrap();

    let sockets = SocketSet::new(vec![]);

    let connection_map = HashMap::<usize, SocketHandle>::new();

    let endpoint: SocketAddr = conf
        .get_from(Some("Peer"), "Endpoint")
        .unwrap()
        .parse()
        .unwrap();

    (VpnEngine {
        tunn: tunnel,
        iface: iface,
        sockets: sockets,
        device: device,
        connection_map: connection_map,
        endpoint: endpoint,
        udp_send_channel: udp_send_channel,
        udp_recv_channel: udp_recv_channel,
        device_rx_sender: device_rxqueue_s,
        device_tx_receiver: device_txqueue_r,
        control_receiver: control_receiver
    }, control_sender)
}
pub fn setup_smoltcp_iface(
    conf: Ini,
    rx_queue: mpsc::Receiver<Vec<u8>>,
    tx_queue: mpsc::Sender<Vec<u8>>,
) -> Result<(Interface, MyVirtualDevice)> {
    let config = Config::new(HardwareAddress::Ip);

    let mut device = MyVirtualDevice {
        rx_queue: rx_queue,
        tx_queue: tx_queue,
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
