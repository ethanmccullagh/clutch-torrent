use base64::{engine::general_purpose, Engine as _};
use boringtun::noise::TunnResult;
use etherparse::PacketBuilder;
use ini::Ini;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium};
use smoltcp::wire::{HardwareAddress, IpAddress, IpCidr};
use std::net::UdpSocket;
use tokio::signal;
use std::time::{Duration, Instant};

mod setup;
mod test;
use setup::*;

/// A simple Virtual Device that stores packets in a Vec

//#[tokio::main]
fn main() {
    let conf = Ini::load_from_file("/home/ethanm/Downloads/cachy-CA-1149.conf").unwrap();

    let iface = setup_smoltcp_iface(conf.clone()).unwrap();
    let mut tunnel = match setup_tunnel(conf.clone()) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("Tunn Init error {}", e);
            return;
        }
    };

    let udp_socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    udp_socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();


}
