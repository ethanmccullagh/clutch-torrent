use smoltcp::wire::Ipv4Address;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, Notify};

use crate::schema::{ProxyMessage, WriteMessage};

async fn run_socks5_server(
    listener: TcpListener,
    control_sender: mpsc::Sender<ProxyMessage>,
) {
    loop {
        if let Ok((mut stream, _)) = listener.accept().await {
            let control_clone = control_sender.clone();

            // 1. SOCKS5 Greeting: [Version, NumMethods, Methods...]
            let mut buf = [0u8; 2];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(&[0x05, 0x00]).await.unwrap(); // No Auth

            // 2. SOCKS5 Connection Request
            let mut req = [0u8; 10];
            stream.read_exact(&mut req).await.unwrap();

            // Extract destination IP from SOCKS5 request (simplified)
            let dest_ip = Ipv4Address::from_bytes(&req[4..8]);
            let dest_port = u16::from_be_bytes([req[8], req[9]]);

            // 3. Create smoltcp Socket & Bridge
            // (You would send a command to your main engine to create this socket)
            bridge_socks_to_smoltcp(stream, dest_ip, dest_port, control_clone).await;
        }
    }
}

async fn bridge_socks_to_smoltcp(
    mut socks_stream: TcpStream,
    dest_ip: Ipv4Address,
    dest_port: u16,
    control_sender: mpsc::Sender<ProxyMessage>,
) {
    // 1. Tell the smoltcp engine to connect
    // ... logic to add a TcpSocket to the SocketSet ...

    let mut buf = [0u8; 8192];
    loop {
        // READ from SOCKS5 -> WRITE to smoltcp
        if let Ok(res) = socks_stream.read(&mut buf).await {
            let n = res;
            if n == 0 {
                break;
            }

            // Push into smoltcp buffer
            let (s, r) = oneshot::channel::<bool>();
            let _ = control_sender.send(ProxyMessage::Write {
                msg: WriteMessage::TCP {},
            }).await;
            // "Interrupt" the engine to send the packet
        }

        // READ from smoltcp -> WRITE to SOCKS5
        // Using poll_fn lets us sleep until smoltcp actually has data
        let (s, r) = oneshot::channel::<bool>();
        let _ = control_sender.send(ProxyMessage::Write {
            msg: WriteMessage::TCP {},
        }).await;
    }
}
