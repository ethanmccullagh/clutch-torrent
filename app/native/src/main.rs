use ini::Ini;
use smoltcp::time::Instant;
use tokio::net::unix::pipe::Receiver;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use std::thread::JoinHandle;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;
pub mod engine;
pub mod helper;
pub mod schema;
pub mod setup;
pub mod socks;
pub mod test;

//mod test;
use helper::create_icmp_packet;
use schema::ChannelMessage;
use setup::*;
use socks::socks_listen;
use test::*;

use crate::schema::VpnEngine;

#[tokio::main]
async fn main() -> io::Result<()> {
    //load config file from path
    let conf = Ini::load_from_file("/home/ethanm/Downloads/cachy-CA-1149.conf").unwrap();

    

    let (udp_recv_s, udp_recv_r) = mpsc::channel::<ChannelMessage>(100);
    let (udp_transmit_s, udp_transmit_r) = mpsc::channel::<ChannelMessage>(100);
    let udp_socket = UdpSocket::bind("0.0.0.0:0").await?;

    let mut engine = setup_engine(conf, udp_transmit_s, udp_recv_r).await;

    let socket_r = Arc::new(udp_socket);
    let socket_s = socket_r.clone();

    let _ = tokio::spawn(udp_recv_task(socket_r, udp_recv_s));
    let _ = tokio::spawn(udp_transmit_task(socket_s, udp_transmit_r));

    // let listener = TcpListener::bind("127.0.0.1:1080").await;

    //TEST: ping google using smoltcp interface
    let _ = interface_test(&mut engine).await;

    Ok(())
}

async fn udp_recv_task(
    socket: Arc<UdpSocket>,
    sender: mpsc::Sender<ChannelMessage>,
) -> io::Result<()> {
    let mut buffer = [0u8; 4096];

    loop {
        //println!("-->RECEIVE: waiting");
        if let Ok((len, addr)) = socket.recv_from(&mut buffer).await{
            println!("-->RECEIVE: packet received from network");
            sender.send(ChannelMessage{data: buffer[..len].to_vec(), addr: addr}).await.unwrap();
            println!("-->RECEIVE: packet sent to channel");

        }
    }
}

async fn udp_transmit_task(
    socket: Arc<UdpSocket>,
    mut receiver: mpsc::Receiver<ChannelMessage>,
) -> io::Result<()> {
    loop {
        //println!("-->TRANSMIT: waiting");
        if let Some(msg) = receiver.recv().await {
            let bytes = msg.data;
            let addr = msg.addr;
            println!("-->TRANSMIT: packet received from channel");
            let _ = socket.send_to(&bytes.as_slice(), &addr).await;
            println!("-->TRANSMIT: packet sent to network");
        }
    }
}
