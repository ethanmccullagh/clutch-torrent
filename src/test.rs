use crate::helper::*;
use crate::schema::{ConnectMessage, ProxyMessage, SocketReadChannel, WriteMessage};
use bytes::Bytes;
use smoltcp::wire::{IpAddress, IpEndpoint};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::boringthreads::spawn_boring_threads;
    use crate::engine::run_engine;
    use crate::setup::setup_tunnel;
    use crate::socks5::run_server;

    // Every test opens its own WireGuard session with the same key; the server keeps one session per peer, so parallel tests break each other.
    static TUNNEL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    use ini::Ini;
    use std::env;

    const MY_VPN_IP: [u8; 4] = [10, 2, 0, 2];
    const TARGET_IP: [u8; 4] = [8, 8, 8, 8];

    async fn start_test_environment() -> (mpsc::Sender<ProxyMessage>, mpsc::Sender<Vec<u8>>) {
        let config_path = env::var("WG_CONFIG_PATH")
            .unwrap_or_else(|_| "cachy3-CA-1208.conf".to_string());
        let conf = Ini::load_from_file(config_path).expect("Failed to load WG config for testing");

        let (vpn_sender, vpn_receiver) = mpsc::channel::<Vec<u8>>(100);
        let (device_sender, device_receiver) = mpsc::channel::<Vec<u8>>(100);
        let (control_sender, control_receiver) = mpsc::channel::<ProxyMessage>(100);

        let (tunn, endpoint) = setup_tunnel(conf.clone());
        spawn_boring_threads(tunn, endpoint, vpn_receiver, device_sender.clone())
            .await
            .expect("Failed to spawn boring threads");

        let vpn_sender_clone = vpn_sender.clone();
        std::thread::spawn(move || {
            run_engine(conf, device_receiver, vpn_sender_clone, control_receiver);
        });

        // Start SOCKS5 server as well
        let control_sender_clone = control_sender.clone();
        tokio::spawn(async move {
            let _ = run_server(control_sender_clone).await;
        });

        // Give it a moment to initialize
        tokio::time::sleep(Duration::from_millis(1000)).await;

        (control_sender, vpn_sender)
    }

    #[tokio::test]
    async fn test_ping() {
        let _tunnel = TUNNEL_LOCK.lock().await;
        let config_path = env::var("WG_CONFIG_PATH")
            .unwrap_or_else(|_| "cachy3-CA-1208.conf".to_string());
        let conf = Ini::load_from_file(config_path).expect("Failed to load WG config for testing");

        let (vpn_sender, vpn_receiver) = mpsc::channel::<Vec<u8>>(100);
        let (device_sender, mut device_receiver) = mpsc::channel::<Vec<u8>>(100);

        let (tunn, endpoint) = setup_tunnel(conf);
        let _handles = spawn_boring_threads(tunn, endpoint, vpn_receiver, device_sender)
            .await
            .expect("Failed to spawn boring threads");

        let ping_packet = create_icmp_packet(MY_VPN_IP, TARGET_IP);

        vpn_sender.send(ping_packet).await.unwrap();

        let mut success = false;
        let timeout = tokio::time::sleep(Duration::from_secs(5));
        tokio::pin!(timeout);

        loop {
            tokio::select! {
                msg = device_receiver.recv() => {
                    if let Some(data) = msg {
                        if parse_and_print_icmp(data.as_slice()) {
                            success = true;
                            break;
                        }
                    }
                }
                _ = &mut timeout => break,
            }
        }
        assert!(success, "Ping response not received or invalid");
    }

    #[tokio::test]
    async fn test_icmp_socket() {
        let _tunnel = TUNNEL_LOCK.lock().await;
        let (control_sender, _) = start_test_environment().await;

        let (s, r) = oneshot::channel();
        let (send, mut read) = mpsc::channel::<SocketReadChannel>(10);

        control_sender
            .send(ProxyMessage::Connect {
                msg: ConnectMessage::ICMP {
                    resp: s,
                    read: send,
                },
            })
            .await
            .unwrap();

        let handle = r.await.unwrap().expect("Failed to create ICMP socket");

        control_sender
            .send(ProxyMessage::Write {
                msg: WriteMessage::ICMP {
                    handle,
                    dst: [8, 8, 8, 8],
                    ident: 0x1234,
                    seq: 1,
                },
            })
            .await
            .unwrap();

        let success = tokio::time::timeout(Duration::from_secs(5), async {
            if let Some(Some((data, addr))) = read.recv().await {
                return parse_and_print_icmp_noip(&data, addr);
            }
            false
        })
        .await
        .unwrap_or(false);

        assert!(success, "ICMP response failed");

        control_sender.send(ProxyMessage::Close).await.unwrap();
    }

    #[tokio::test]
    async fn test_tcp_socket() {
        let _tunnel = TUNNEL_LOCK.lock().await;
        let (control_sender, _) = start_test_environment().await;

        let (s, r) = oneshot::channel::<Option<smoltcp::iface::SocketHandle>>();
        let (send, mut read) = mpsc::channel::<SocketReadChannel>(10);

        control_sender
            .send(ProxyMessage::Connect {
                msg: ConnectMessage::TCP {
                    remote: IpEndpoint::new(IpAddress::v4(45, 79, 112, 203), 4242),
                    local_port: 0,
                    connect: true,
                    resp: s,
                    read: send,
                },
            })
            .await
            .unwrap();

        let handle: smoltcp::iface::SocketHandle =
            r.await.unwrap().expect("TCP socket creation failed");

        let success = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let _ = control_sender
                    .send(ProxyMessage::Write {
                        msg: WriteMessage::TCP {
                            handle,
                            data: Bytes::copy_from_slice(b"Hello TCP\n"),
                        },
                    })
                    .await;

                match read.recv().await {
                    Some(Some((data, _addr))) => {
                        return &b"Hello TCP\n"[..] == &data;
                    }
                    Some(None) => {
                        //socket isnt open for write yet
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    None => {
                        return false;
                    }
                }
            }
        })
        .await
        .unwrap_or(false);

        assert!(success, "TCP echo test failed");

        control_sender.send(ProxyMessage::Close).await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires real VPN config and network access"]
    async fn test_socks5_proxy() {
        let _tunnel = TUNNEL_LOCK.lock().await;
        let (control_sender, _) = start_test_environment().await;

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let mut stream = TcpStream::connect("127.0.0.1:1080")
            .await
            .expect("Failed to connect to SOCKS5 proxy");

        // SOCKS5 handshake
        stream.write_all(&[5, 1, 0]).await.unwrap();
        let mut resp = [0u8; 2];
        stream.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp[0], 5);
        assert_eq!(resp[1], 0);

        // CONNECT to 8.8.8.8:80
        stream
            .write_all(&[
                5, 1, 0, 1, // VER, CMD=CONNECT, RSV, ATYP=IPv4
                8, 8, 8, 8, // IP: 8.8.8.8
                0, 80, // Port: 80
            ])
            .await
            .unwrap();

        let mut connect_resp = [0u8; 10];
        stream.read_exact(&mut connect_resp).await.unwrap();
        assert_eq!(connect_resp[1], 0, "SOCKS5 CONNECT failed");

        // Send HTTP request
        stream
            .write_all(b"GET / HTTP/1.0\r\nHost: 8.8.8.8\r\n\r\n")
            .await
            .unwrap();

        // Read response
        let mut buffer = [0u8; 1024];
        let n = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer))
            .await
            .unwrap()
            .unwrap();

        assert!(n > 0, "No data received from SOCKS5 proxy");

        control_sender.send(ProxyMessage::Close).await.unwrap();
    }
}

pub fn ping_test(
    vpn_sender: &mpsc::Sender<Vec<u8>>,
    device_receiver: &mut mpsc::Receiver<Vec<u8>>,
) {
    let ping_packet = create_icmp_packet([10, 2, 0, 2], [8, 8, 8, 8]);
    let mut reponse_read = true;

    loop {
        if reponse_read {
            println!("TEST: Sending ping to google");
            let _ = vpn_sender.try_send(ping_packet.clone());
            reponse_read = false;
        }
        while let Ok(msg) = device_receiver.try_recv() {
            reponse_read = true;
            if parse_and_print_icmp(msg.as_slice()) {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

pub async fn socket_test(control_sender: mpsc::Sender<ProxyMessage>) {
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

    if let Some(handle) = r.await.unwrap() {
        loop {
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
            if let Ok(Some(Some((data, addr)))) =
                tokio::time::timeout(Duration::from_secs(2), read.recv()).await
            {
                if parse_and_print_icmp_noip(&data, addr) {
                    break;
                }
            }
        }
    }
}

pub async fn tcp_socket_test(control_sender: mpsc::Sender<ProxyMessage>) {
    let (s, r) = oneshot::channel();
    let (send, mut read) = mpsc::channel::<SocketReadChannel>(10);

    let _ = control_sender
        .send(ProxyMessage::Connect {
            msg: ConnectMessage::TCP {
                remote: IpEndpoint::new(IpAddress::v4(45, 79, 112, 203), 4242),
                local_port: 0,
                connect: true,
                resp: s,
                read: send,
            },
        })
        .await;

    if let Some(handle) = r.await.unwrap() {
        let _ = control_sender
            .send(ProxyMessage::Write {
                msg: WriteMessage::TCP {
                    handle,
                    data: Bytes::copy_from_slice(b"Hello TCP\n"),
                },
            })
            .await;
        if let Ok(Some(Some((data, _)))) =
            tokio::time::timeout(Duration::from_secs(5), read.recv()).await
        {
            if &b"Hello TCP\n"[..] == data {
                println!("TCP Test Passed !");
            }
        }
    }
}

pub async fn udp_socket_test(control_sender: mpsc::Sender<ProxyMessage>) {
    let (s, r) = oneshot::channel();
    let (send, mut read) = mpsc::channel::<SocketReadChannel>(10);

    let _ = control_sender
        .send(ProxyMessage::Connect {
            msg: ConnectMessage::UDP {
                local_port: 5000,
                resp: s,
                read: send,
            },
        })
        .await;

    if let Some(handle) = r.await.unwrap() {
        let _ = control_sender
            .send(ProxyMessage::Write {
                msg: WriteMessage::NAT {
                    handle,
                    opcode: 0x0,
                },
            })
            .await;
        if let Ok(Some(Some((payload, _addr)))) =
            tokio::time::timeout(Duration::from_secs(5), read.recv()).await
        {
            println!("UDP response received: {:?}", payload);
        }
    }
}

pub async fn socks5_test() -> Result<(), Box<dyn std::error::Error>> {
    Ok(())
}
