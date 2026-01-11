use crate::schema::VpnEngine;
use crate::{helper::*, schema::ProxyMessage};
use boringtun::noise::TunnResult;
use smoltcp::iface::SocketHandle;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::thread::sleep;
use tokio::net::{TcpListener, TcpSocket};
use tokio::sync::{mpsc, oneshot};

use smoltcp::socket::tcp;
use std::time::{Duration, Instant};

// --- CONFIGURATION (Replace these with your ProtonVPN details) ---
const MY_VPN_IP: [u8; 4] = [10, 2, 0, 2]; // Your interface Address (e.g. 10.2.0.2)
const TARGET_IP: [u8; 4] = [8, 8, 8, 8]; // Google DNS

//inject icmp packet directly into smoltcp transmit queue
//test boringtun/udp socket logic
//by-passes smoltcp interface entirely
pub fn ping_test(engine: &mut VpnEngine) {
    let ping_packet = create_icmp_packet(MY_VPN_IP, TARGET_IP);
    let mut reponse_read = true;
    let sender = engine.device.vpn_tx.clone();
    let receiver = &mut engine.device.rx_queue;

    loop {
        //Send google ping

        if reponse_read {
            println!("TEST: Sending ping to google");
            let _ = sender.try_send(ping_packet.clone());
            reponse_read = false;
        }
        println!("TEST: waiting on response...");
        while let Ok(msg) = receiver.try_recv() {
            if !reponse_read {
                reponse_read = true
            }

            println!("TEST: Response Received!");
            if parse_and_print_icmp(msg.as_slice()) {
                println!("TEST: Done!");
                return;
            }
        }
        sleep(Duration::from_millis(200));
    }
}

pub async fn connection_test(control_sender: mpsc::Sender<ProxyMessage>) {
    let (callback_send, callback_receive) = oneshot::channel();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_addr = listener.local_addr().unwrap();
    let port = proxy_addr.port();

    println!("TCP listener created on port: {:?}", proxy_addr);

    let mut connection_map = HashMap::<u64, SocketHandle>::new();

    let _ = control_sender
        .send(ProxyMessage::Connect {
            addr: [127, 0, 0, 1],
            port: port,
            resp: callback_send,
        })
        .await;

    println!("TEST: waiting on accept: {:?}", port);

    match listener.accept().await {
        Ok((stream, addr)) => {
            println!("TCP stream confirmed: {:?}", stream);
            println!("TCP stream addr: {:?}", addr);
        }
        Err(e) => println!("TEST: accept failed {:?}", e),
    }

    if let Some(handle) = callback_receive.await.unwrap() {
        let mut hasher = DefaultHasher::new();
        port.hash(&mut hasher);
        connection_map.insert(hasher.finish(), handle);
    } else {
        println!("Callback failed: None returned");
    }
}

//Same google ping as before
//Now packet is sent through tcp socket connected to smoltcp interface
//Verifies socket control and smoltcp internal logic
pub async fn socket_test(control_sender: mpsc::Sender<ProxyMessage>) {

    //create a new socket

    //send connection message

    //confirm connection

    //start sending test packets to socket

    //await reponse on socket
}
