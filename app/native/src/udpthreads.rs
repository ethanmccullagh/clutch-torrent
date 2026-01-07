use crate::schema::*;
use anyhow::Result;
use std::sync::Arc;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub async fn spawn_udp_threads() -> Result<(
    mpsc::Sender<ChannelMessage>,
    mpsc::Receiver<ChannelMessage>,
    Vec<JoinHandle<()>>,
)> {
    let (udp_recv_s, udp_recv_r) = mpsc::channel::<ChannelMessage>(100);
    let (udp_transmit_s, udp_transmit_r) = mpsc::channel::<ChannelMessage>(100);
    let udp_socket = UdpSocket::bind("0.0.0.0:0").await?;

    let socket_r = Arc::new(udp_socket);
    let socket_s = socket_r.clone();
    let mut task_handles = Vec::new();

    let th1 = tokio::spawn(udp_recv_task(socket_r, udp_recv_s));
    let th2 = tokio::spawn(udp_transmit_task(socket_s, udp_transmit_r));

    task_handles.push(th1);
    task_handles.push(th2);
    Ok((udp_transmit_s, udp_recv_r, task_handles))
}

async fn udp_recv_task(socket: Arc<UdpSocket>, sender: mpsc::Sender<ChannelMessage>) {
    let mut buffer = [0u8; 4096];

    loop {
        //println!("-->RECEIVE: waiting");
        if let Ok((len, addr)) = socket.recv_from(&mut buffer).await {
            println!("-->RECEIVE: packet received from network");
            sender
                .send(ChannelMessage {
                    data: buffer[..len].to_vec(),
                    addr: addr,
                })
                .await
                .unwrap();
            println!("-->RECEIVE: packet sent to channel");
        }
    }
}

async fn udp_transmit_task(socket: Arc<UdpSocket>, mut receiver: mpsc::Receiver<ChannelMessage>) {
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
