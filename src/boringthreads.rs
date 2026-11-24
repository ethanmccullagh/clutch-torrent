use anyhow::Result;
use boringtun::noise::{Tunn, TunnResult};
use core::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

pub async fn spawn_boring_threads(
    tunn: Tunn,
    endpoint: SocketAddr,
    vpn_receiver: mpsc::Receiver<Vec<u8>>,
    device_sender: mpsc::Sender<Vec<u8>>,
) -> Result<Vec<JoinHandle<()>>> {
    let udp_socket = UdpSocket::bind("0.0.0.0:0").await?;

    let tunn1 = Arc::new(Mutex::new(tunn));
    let tunn2 = tunn1.clone();

    let socket_r = Arc::new(udp_socket);
    let socket_s = socket_r.clone();

    let th1 = tokio::spawn(udp_recv_task(
        socket_r,
        device_sender,
        endpoint.clone(),
        tunn1,
    ));
    let th2 = tokio::spawn(udp_transmit_task(
        socket_s,
        vpn_receiver,
        endpoint.clone(),
        tunn2,
    ));

    let mut task_handles = Vec::new();
    task_handles.push(th1);
    task_handles.push(th2);
    Ok(task_handles)
}

async fn udp_recv_task(
    socket: Arc<UdpSocket>,
    sender: mpsc::Sender<Vec<u8>>,
    endpoint: SocketAddr,
    t: Arc<Mutex<Tunn>>,
) {
    let mut buffer = [0u8; 4096];
    let mut decrypted_buf = [0u8; 4096];

    loop {
        //println!("-->RECEIVE: waiting");
        let (len, addr) = match socket.recv_from(&mut buffer).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("WireGuard UDP recv error: {e}");
                continue;
            }
        };
        {
            //println!("-->BORING RECEIVE: packet received from network");

            if addr != endpoint {
                println!("-->RECEIVE: wrong endpoint");
                continue;
            }

            let mut tunn = t.lock().await;
            let mut result = tunn.decapsulate(None, &mut buffer[..len], &mut decrypted_buf);
            loop {
                match result {
                    TunnResult::Done => {
                        //println!("-->RECEIVE: Done!");
                        break;
                    }
                    TunnResult::WriteToNetwork(b) => {
                        //print_packet(b);
                        if let Err(e) = socket.send_to(b, addr).await {
                            eprintln!("WireGuard UDP send to {addr} failed: {e}");
                        }

                        //println!("-->Boring RECEIVE: packet sent to network");
                        result = tunn.decapsulate(None, &[], &mut decrypted_buf);
                    }
                    TunnResult::WriteToTunnelV4(b, _) => {
                        //parse_and_print_icmp(b);

                        //Drop Proton MTU Probe
                        // if b.len() > 20 && !b.windows(16).any(|w| w == b"Proton VPN MTU P") {
                        //     println!("Boring Tunn Recv-----------");
                        //     parse_and_print_tcp(&b);
                        // }

                        if sender.send(b.to_vec()).await.is_err() {
                            eprintln!("engine channel closed, stopping WireGuard receive task");
                            return;
                        }
                        //println!("-->BORING RECEIVE: packet sent to channel");
                        result = tunn.decapsulate(None, &[], &mut decrypted_buf);
                    }
                    TunnResult::Err(e) => {
                        eprintln!("WireGuard decapsulate error: {e:?}");
                        break;
                    }
                    TunnResult::WriteToTunnelV6(..) => {
                        eprintln!("dropping IPv6 packet from tunnel: interface is IPv4 only");
                        break;
                    }
                }
            }

            drop(tunn);
        }
    }
}

async fn udp_transmit_task(
    socket: Arc<UdpSocket>,
    mut receiver: mpsc::Receiver<Vec<u8>>,
    endpoint: SocketAddr,
    t: Arc<Mutex<Tunn>>,
) {
    let mut buffer = [0u8; 4096];
    loop {
        //println!("-->TRANSMIT: waiting");
        // None means the engine dropped its sender; looping on it would spin forever.
        let Some(msg) = receiver.recv().await else {
            eprintln!("engine channel closed, stopping WireGuard transmit task");
            return;
        };
        {
            //println!("-->BORING TRANSMIT: packet received from channel");

            let mut tunn = t.lock().await;

            match tunn.encapsulate(msg.as_slice(), &mut buffer) {
                TunnResult::WriteToNetwork(encrypted_bytes) => {
                    if let Err(e) = socket.send_to(encrypted_bytes, &endpoint).await {
                        eprintln!("WireGuard UDP send to {endpoint} failed: {e}");
                    }
                    //println!("-->BORING TRANSMIT: packet sent to network");
                }
                TunnResult::Err(e) => eprintln!("WireGuard encapsulate error: {e:?}"),
                _ => {}
            }

            drop(tunn);
        }
    }
}
