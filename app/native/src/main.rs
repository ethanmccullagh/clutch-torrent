use ini::Ini;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

pub mod test;
pub mod helper;
pub mod setup;

//mod test;
use test::*;
use setup::*;

use crate::helper::get_endpoint;

/// A simple Virtual Device that stores packets in a Vec

//#[tokio::main]
fn main() {

    //load config file from path
    let conf = Ini::load_from_file("/home/ethanm/Downloads/cachy-CA-1149.conf").unwrap();

    let mut tunnel = setup_tunnel(conf.clone()).unwrap();

    let iface = setup_smoltcp_iface(conf.clone()).unwrap();

    let endpoint = get_endpoint(conf);

    let udp_socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    udp_socket.set_read_timeout(Some(Duration::from_millis(100))).unwrap();

    //ping google to test tunnel
    let _ = match google_ping_test(&mut tunnel, udp_socket, endpoint) {
        Ok(()) => (),
        Err(e) => {
            eprint!("Google Ping test failed: {}", e);
            return
        }
    };

}
