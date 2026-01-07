use crate::schema::VpnEngine;
use crate::{helper::*, schema::ProxyMessage};
use boringtun::noise::TunnResult;
use tokio::sync::mpsc;

use smoltcp::socket::tcp;
use std::time::{Duration, Instant};

// --- CONFIGURATION (Replace these with your ProtonVPN details) ---
const MY_VPN_IP: [u8; 4] = [10, 2, 0, 2]; // Your interface Address (e.g. 10.2.0.2)
const TARGET_IP: [u8; 4] = [8, 8, 8, 8]; // Google DNS

//inject icmp packet directly into smoltcp transmit queue
//test boringtun/udp socket logic
//by-passes smoltcp interface entirely
pub fn ping_test() {}

//Same google ping as before
//Now packet is sent through tcp socket connected to smoltcp interface
//Verifies socket control and smoltcp internal logic
pub async fn socket_test(control_sender: mpsc::Sender<ProxyMessage>) {
    let ping_packet = create_icmp_packet(MY_VPN_IP, TARGET_IP);

    //create a new socket

    //send connection message

    //confirm connection

    //start sending test packets to socket

    //await reponse on socket
}
