use anyhow::Result;
use std::net::{SocketAddr, ToSocketAddrs};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use crate::schema::{ConnectMessage, ProxyMessage, SocketReadChannel, WriteMessage};
use smoltcp::wire::{IpAddress, IpEndpoint};
use std::str::FromStr;

pub async fn run_server(proxy_tx: mpsc::Sender<ProxyMessage>) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:1080").await?;
    println!("SOCKS5 server listening on 127.0.0.1:1080");

    loop {
        let (stream, _) = listener.accept().await?;
        let proxy_tx = proxy_tx.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, proxy_tx).await {
                eprintln!("SOCKS5 error: {}", e);
            }
        });
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    proxy_tx: mpsc::Sender<ProxyMessage>,
) -> Result<()> {
    // SOCKS5 handshake
    let mut handshake_buf = [0u8; 2];
    stream.read_exact(&mut handshake_buf).await?;

    if handshake_buf[0] != 5 {
        return Err(anyhow::anyhow!("Invalid SOCKS5 version"));
    }

    let nmethods = handshake_buf[1] as usize;
    let mut methods = vec![0u8; nmethods];
    stream.read_exact(&mut methods).await?;

    // We only support NO AUTHENTICATION REQUIRED
    if !methods.contains(&0) {
        return Err(anyhow::anyhow!("No supported authentication methods"));
    }

    // Send NO AUTHENTICATION
    stream.write_all(&[5, 0]).await?;

    // SOCKS5 request
    let mut request_header = [0u8; 4];
    stream.read_exact(&mut request_header).await?;

    if request_header[0] != 5 {
        return Err(anyhow::anyhow!("Invalid SOCKS5 version"));
    }

    let command = request_header[1];
    let atyp = request_header[3];

    if command != 1 {
        // We only support CONNECT
        // Send a "Command not supported" reply
        let reply = [
            5, // SOCKS5 version
            7, // Command not supported
            0, // Reserved
            1, // IPv4 address type
            0, 0, 0, 0, // 0.0.0.0
            0, 0, // Port 0
        ];
        stream.write_all(&reply).await?;
        return Err(anyhow::anyhow!("Unsupported command: {}", command));
    }

    let addr = match atyp {
        1 => {
            // IPv4
            let mut addr_buf = [0u8; 4];
            stream.read_exact(&mut addr_buf).await?;
            let port = stream.read_u16().await?;
            let addr = std::net::Ipv4Addr::from(addr_buf);
            SocketAddr::new(std::net::IpAddr::V4(addr), port)
        }
        3 => {
            // Domain name
            let len = stream.read_u8().await? as usize;
            let mut domain_buf = vec![0u8; len];
            stream.read_exact(&mut domain_buf).await?;
            let port = stream.read_u16().await?;
            let domain = String::from_utf8(domain_buf)?;
            let mut addrs = (domain.as_str(), port).to_socket_addrs()?;
            addrs
                .next()
                .ok_or_else(|| anyhow::anyhow!("Could not resolve domain"))?
        }
        4 => {
            // IPv6
            let mut addr_buf = [0u8; 16];
            stream.read_exact(&mut addr_buf).await?;
            let port = stream.read_u16().await?;
            let addr = std::net::Ipv6Addr::from(addr_buf);
            SocketAddr::new(std::net::IpAddr::V6(addr), port)
        }
        _ => {
            return Err(anyhow::anyhow!("Invalid address type"));
        }
    };

    let (resp_tx, resp_rx) = oneshot::channel();
    let (read_tx, mut read_rx) = mpsc::channel::<SocketReadChannel>(100);

    let remote_ip = match addr.ip() {
        std::net::IpAddr::V4(ipv4) => IpAddress::from_str(&ipv4.to_string())
            .map_err(|_| anyhow::anyhow!("Invalid IPv4 address"))?,
        std::net::IpAddr::V6(ipv6) => IpAddress::from_str(&ipv6.to_string())
            .map_err(|_| anyhow::anyhow!("Invalid IPv6 address"))?,
    };

    let connect_msg = ConnectMessage::TCP {
        remote: IpEndpoint::new(remote_ip, addr.port()),
        local_port: 0, // Let the engine choose
        connect: true,
        resp: resp_tx,
        read: read_tx,
    };

    proxy_tx
        .send(ProxyMessage::Connect { msg: connect_msg })
        .await?;

    let handle = match resp_rx.await? {
        Some(handle) => handle,
        None => {
            // Connection failed
            let reply = [
                5, // SOCKS5 version
                4, // Host unreachable
                0, // Reserved
                1, // IPv4 address type
                0, 0, 0, 0, // 0.0.0.0,
                0, 0, // Port 0
            ];
            stream.write_all(&reply).await?;
            return Err(anyhow::anyhow!("Connection failed"));
        }
    };

    let reply = [
        5, // SOCKS5 version
        0, // Success
        0, // Reserved
        1, // IPv4 address type
        0, 0, 0, 0, // 0.0.0.0,
        0, 0, // Port 0
    ];
    stream.write_all(&reply).await?;

    let (mut reader, mut writer) = stream.split();

    // Read from client and send to engine
    let f1 = async move {
        let mut buf = [0u8; 4096];
        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                return Ok::<(), anyhow::Error>(());
            }
            proxy_tx
                .send(ProxyMessage::Write {
                    msg: WriteMessage::TCP {
                        handle,
                        data: bytes::Bytes::copy_from_slice(&buf[0..n]),
                    },
                })
                .await?;
        }
    };

    // Read from engine and send to client
    let f2 = async move {
        while let Some(Some((data, _))) = read_rx.recv().await {
            writer.write_all(&data).await?;
        }
        Ok::<(), anyhow::Error>(())
    };

    tokio::try_join!(f1, f2)?;

    Ok(())
}

