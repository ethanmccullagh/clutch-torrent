use crate::helper::*;
use crate::schema::{ConnectMessage, ProxyMessage, SocketReadChannel, WriteMessage};
use smoltcp::wire::IpAddress;
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time;

const MY_VPN_IP: [u8; 4] = [10, 2, 0, 2];
const TARGET_IP: [u8; 4] = [8, 8, 8, 8];

//Unit Test for boringtun
pub fn ping_test(
    vpn_sender: &mpsc::Sender<Vec<u8>>,
    device_receiver: &mut mpsc::Receiver<Vec<u8>>,
) {
    let ping_packet = create_icmp_packet(MY_VPN_IP, TARGET_IP);
    let mut reponse_read = true;
    let mut success = false;

    loop {
        if reponse_read {
            println!("TEST: Sending ping to google");
            let _ = vpn_sender.try_send(ping_packet.clone());
            reponse_read = false;
        }
        println!("TEST: waiting on response...");
        while let Ok(msg) = device_receiver.try_recv() {
            if !reponse_read {
                reponse_read = true
            }

            println!("TEST: Response Received!");
            if parse_and_print_icmp(msg.as_slice()) {
                success = true;
            } else {
                sleep(Duration::from_millis(1000));
            }
        }

        if success {
            println!("TEST: Done!");
            return;
        }
        sleep(Duration::from_millis(200));
    }
}
//Same google ping as before
//Now packet is sent through tcp socket connected to smoltcp interface
//Verifies socket control and smoltcp internal logic
//
//Sends a ping every 2 seconds
pub async fn socket_test(control_sender: mpsc::Sender<ProxyMessage>) {
    //send connection message

    let (s, r) = oneshot::channel();
    let (send, mut read) = mpsc::channel::<SocketReadChannel>(10);

    let _ = control_sender
        .send(ProxyMessage::Connect {
            msg: ConnectMessage::ICMP {
                resp: s,
                read: send,
            },
        })
        .await;
    //println!("ICMP socket created");

    if let Some(handle) = r.await.unwrap() {
        let mut send_ping = true;
        loop {
            if send_ping {
                let _ = control_sender
                    .send(ProxyMessage::Write {
                        msg: WriteMessage::ICMP {
                            handle,
                            dst: [8, 8, 8, 8],
                            ident: 0x1234,
                            seq: 1,
                        },
                    })
                    .await;
                send_ping = false;
            }
            let _ = tokio::select! {
                r = read.recv() => {
                    if let Some((data, addr)) = r {
                        println!("ICMP response received!");
                        parse_and_print_icmp_noip(&data, addr);
                    }
                }
                _ =  time::sleep(Duration::from_millis(2000)) => {
                    send_ping = true;
                }

            };
        }
    }

    //start sending test packets to socket

    //await reponse on socket
}

//Test SOCKS5 proxy by connecting through it
pub async fn socks5_test() -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    println!("TEST: Connecting to SOCKS5 proxy at 127.0.0.1:1080");
    let mut stream = TcpStream::connect("127.0.0.1:1080").await?;

    // SOCKS5 handshake
    stream.write_all(&[5, 1, 0]).await?;
    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp).await?;

    if resp[0] != 5 || resp[1] != 0 {
        return Err("SOCKS5 handshake failed".into());
    }
    println!("TEST: SOCKS5 handshake successful");

    // CONNECT to 8.8.8.8:80
    stream
        .write_all(&[
            5, 1, 0, 1, // VER, CMD=CONNECT, RSV, ATYP=IPv4
            8, 8, 8, 8, // IP: 8.8.8.8
            0, 80, // Port: 80
        ])
        .await?;

    let mut connect_resp = [0u8; 10];
    stream.read_exact(&mut connect_resp).await?;

    if connect_resp[0] != 5 || connect_resp[1] != 0 {
        return Err(format!("SOCKS5 CONNECT failed: {}", connect_resp[1]).into());
    }
    println!("TEST: SOCKS5 CONNECT to 8.8.8.8:80 successful!");

    // Send HTTP request
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: 8.8.8.8\r\n\r\n")
        .await?;
    println!("TEST: Sent HTTP request");

    // Read response
    let mut buffer = [0u8; 1024];
    match tokio::time::timeout(
        tokio::time::Duration::from_secs(5),
        stream.read(&mut buffer),
    )
    .await
    {
        Ok(Ok(n)) if n > 0 => {
            println!("TEST: Received {} bytes response", n);
            println!("TEST: {}", String::from_utf8_lossy(&buffer[..n.min(200)]));
            println!("✅ SOCKS5 to VPN test PASSED!");
            Ok(())
        }
        Ok(Ok(_)) => Err("Connection closed".into()),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err("Timeout waiting for response".into()),
    }
}
