use anyhow::Result;
use std::net::Ipv4Addr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use crate::schema::{ConnectMessage, ProxyMessage, SocketReadChannel, WriteMessage};
use smoltcp::wire::{IpAddress, IpEndpoint};

// SOCKS5 reply codes (RFC 1928)
const SUCCEEDED: u8 = 0;
const HOST_UNREACHABLE: u8 = 4;
const COMMAND_NOT_SUPPORTED: u8 = 7;
const ADDRESS_TYPE_NOT_SUPPORTED: u8 = 8;

/// Reply with `code` and a 0.0.0.0:0 bound address.
async fn reply(stream: &mut TcpStream, code: u8) -> Result<()> {
    stream.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
    Ok(())
}

/// Resolve `name` through the tunnel, so lookups don't leak to the host's DNS.
async fn resolve(proxy_tx: &mpsc::Sender<ProxyMessage>, name: String) -> Result<Option<Ipv4Addr>> {
    let (resp, result) = oneshot::channel();
    proxy_tx.send(ProxyMessage::Resolve { name, resp }).await?;
    Ok(result.await?)
}

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
        reply(&mut stream, COMMAND_NOT_SUPPORTED).await?;
        return Err(anyhow::anyhow!("Unsupported command: {}", command));
    }

    let ip = match atyp {
        1 => {
            let mut addr_buf = [0u8; 4];
            stream.read_exact(&mut addr_buf).await?;
            Ipv4Addr::from(addr_buf)
        }
        3 => {
            let len = stream.read_u8().await? as usize;
            let mut domain_buf = vec![0u8; len];
            stream.read_exact(&mut domain_buf).await?;
            let domain = String::from_utf8(domain_buf)?;
            match resolve(&proxy_tx, domain.clone()).await? {
                Some(ip) => ip,
                None => {
                    reply(&mut stream, HOST_UNREACHABLE).await?;
                    return Err(anyhow::anyhow!("Could not resolve {domain}"));
                }
            }
        }
        4 => {
            // The tunnel interface is IPv4 only.
            reply(&mut stream, ADDRESS_TYPE_NOT_SUPPORTED).await?;
            return Err(anyhow::anyhow!("IPv6 destinations are not supported"));
        }
        _ => {
            reply(&mut stream, ADDRESS_TYPE_NOT_SUPPORTED).await?;
            return Err(anyhow::anyhow!("Invalid address type {atyp}"));
        }
    };
    let port = stream.read_u16().await?;

    let (resp_tx, resp_rx) = oneshot::channel();
    let (read_tx, mut read_rx) = mpsc::channel::<SocketReadChannel>(100);

    let connect_msg = ConnectMessage::TCP {
        remote: IpEndpoint::new(IpAddress::Ipv4(ip.into()), port),
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
            reply(&mut stream, HOST_UNREACHABLE).await?;
            return Err(anyhow::anyhow!("Connection to {ip}:{port} failed"));
        }
    };
    reply(&mut stream, SUCCEEDED).await?;

    let (mut reader, mut writer) = stream.split();

    // Read from client and send to engine
    let f1 = async move {
        let mut buf = [0u8; 4096];
        loop {
            let n = reader.read(&mut buf).await?;
            if n == 0 {
                // Client finished sending. Not passed on as a FIN: Proton's server answers a FIN
                // with a RST, killing the response. Keep relaying the remote's reply instead.
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
        // Remote closed (or the connection failed): pass the EOF on, or the client waits forever.
        writer.shutdown().await?;
        Ok::<(), anyhow::Error>(())
    };

    tokio::try_join!(f1, f2)?;

    Ok(())
}
