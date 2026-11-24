use anyhow::Result;
use boringtun::noise::errors::WireGuardError;
use boringtun::noise::{Tunn, TunnResult};
use core::net::SocketAddr;
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

pub async fn spawn_boring_threads(
    tunn: Tunn,
    endpoint: SocketAddr,
    vpn_receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    device_sender: mpsc::Sender<Vec<u8>>,
) -> Result<Vec<JoinHandle<()>>> {
    let udp_socket = UdpSocket::bind("0.0.0.0:0").await?;

    let tunn1 = Arc::new(Mutex::new(tunn));
    let tunn2 = tunn1.clone();

    let tunn3 = tunn1.clone();

    let socket_r = Arc::new(udp_socket);
    let socket_s = socket_r.clone();
    let socket_t = socket_r.clone();

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

    let th3 = tokio::spawn(timer_task(socket_t, endpoint, tunn3));

    let mut task_handles = Vec::new();
    task_handles.push(th1);
    task_handles.push(th2);
    task_handles.push(th3);
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

                        //loopback to keep unpacking
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

                        //loopback to keep unpacking
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
    mut receiver: mpsc::UnboundedReceiver<Vec<u8>>,
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

/// Drive boringtun's timers: keepalives, rekeys and handshake retransmits. Without this a
/// session is never renewed and the server rejects it after about 3 minutes.
async fn timer_task(socket: Arc<UdpSocket>, endpoint: SocketAddr, t: Arc<Mutex<Tunn>>) {
    // Same tick as boringtun's own device.
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    let mut buffer = [0u8; 4096];
    let mut expired = false;
    loop {
        tick.tick().await;
        let mut tunn = t.lock().await;
        match tunn.update_timers(&mut buffer) {
            TunnResult::Done => expired = false,
            TunnResult::WriteToNetwork(packet) => {
                expired = false;
                if let Err(e) = socket.send_to(packet, &endpoint).await {
                    eprintln!("WireGuard UDP send to {endpoint} failed: {e}");
                }
            }
            // Returned on every tick until new traffic starts a fresh handshake; log it once.
            TunnResult::Err(WireGuardError::ConnectionExpired) => {
                if !expired {
                    eprintln!("WireGuard handshake expired, waiting for traffic to retry");
                }
                expired = true;
            }
            TunnResult::Err(e) => eprintln!("WireGuard timer error: {e:?}"),
            TunnResult::WriteToTunnelV4(..) | TunnResult::WriteToTunnelV6(..) => {}
        }
    }
}
