use anyhow::Result;
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, oneshot, Semaphore};

use crate::schema::{ConnectMessage, ProxyMessage, SocketReadChannel, WriteMessage};
use smoltcp::iface::SocketHandle;
use smoltcp::wire::{IpAddress, IpEndpoint};
use std::sync::Arc;

// SOCKS5 commands and reply codes (RFC 1928)
const CONNECT: u8 = 1;
const UDP_ASSOCIATE: u8 = 3;
const SUCCEEDED: u8 = 0;
const HOST_UNREACHABLE: u8 = 4;
const COMMAND_NOT_SUPPORTED: u8 = 7;
const ADDRESS_TYPE_NOT_SUPPORTED: u8 = 8;

/// A request or UDP datagram's destination address.
enum Target {
    V4(Ipv4Addr),
    Domain(String),
    V6,
}

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

/// The port-forwarded port, and a permit so only one UDP association binds it at a time.
type InboundPort = Option<(u16, Arc<Semaphore>)>;

/// `inbound_port`: the tunnel port mapped for incoming traffic (see `port_forward.rs`). The first
/// UDP association binds its engine socket there, so unsolicited DHT/uTP packets reach the client.
pub async fn run_server(
    proxy_tx: mpsc::Sender<ProxyMessage>,
    inbound_port: Option<u16>,
) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:1080").await?;
    println!("SOCKS5 server listening on 127.0.0.1:1080");
    let inbound: InboundPort = inbound_port.map(|port| (port, Arc::new(Semaphore::new(1))));

    loop {
        let (stream, _) = listener.accept().await?;
        let proxy_tx = proxy_tx.clone();
        let inbound = inbound.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, proxy_tx, inbound).await {
                eprintln!("SOCKS5 error: {}", e);
            }
        });
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    proxy_tx: mpsc::Sender<ProxyMessage>,
    inbound: InboundPort,
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

    if command != CONNECT && command != UDP_ASSOCIATE {
        reply(&mut stream, COMMAND_NOT_SUPPORTED).await?;
        return Err(anyhow::anyhow!("Unsupported command: {}", command));
    }

    let target = match atyp {
        1 => {
            let mut addr_buf = [0u8; 4];
            stream.read_exact(&mut addr_buf).await?;
            Target::V4(Ipv4Addr::from(addr_buf))
        }
        3 => {
            let len = stream.read_u8().await? as usize;
            let mut domain_buf = vec![0u8; len];
            stream.read_exact(&mut domain_buf).await?;
            Target::Domain(String::from_utf8(domain_buf)?)
        }
        4 => {
            stream.read_exact(&mut [0u8; 16]).await?;
            Target::V6
        }
        _ => {
            reply(&mut stream, ADDRESS_TYPE_NOT_SUPPORTED).await?;
            return Err(anyhow::anyhow!("Invalid address type {atyp}"));
        }
    };
    let port = stream.read_u16().await?;

    if command == UDP_ASSOCIATE {
        // The address is where the client will send from. Often 0.0.0.0:0 (unknown); then the
        // client is learned from its first datagram, and datagrams arriving before that are lost.
        let client = match target {
            _ if port == 0 => None,
            Target::V4(ip) if !ip.is_unspecified() => Some(SocketAddr::from((ip, port))),
            _ => Some(SocketAddr::from((Ipv4Addr::LOCALHOST, port))), // relay is loopback only
        };
        return udp_associate(stream, proxy_tx, inbound, client).await;
    }

    let ip = match target {
        Target::V4(ip) => ip,
        Target::Domain(domain) => match resolve(&proxy_tx, domain.clone()).await? {
            Some(ip) => ip,
            None => {
                reply(&mut stream, HOST_UNREACHABLE).await?;
                return Err(anyhow::anyhow!("Could not resolve {domain}"));
            }
        },
        Target::V6 => {
            // The tunnel interface is IPv4 only.
            reply(&mut stream, ADDRESS_TYPE_NOT_SUPPORTED).await?;
            return Err(anyhow::anyhow!("IPv6 destinations are not supported"));
        }
    };

    let (resp_tx, resp_rx) = oneshot::channel();
    let (read_tx, read_rx) = mpsc::channel::<SocketReadChannel>(100);

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
    relay(stream, handle, read_rx, proxy_tx).await
}

/// Copy data both ways between a local TCP stream and an engine TCP socket until both are done.
pub async fn relay(
    mut stream: TcpStream,
    handle: SocketHandle,
    mut read_rx: mpsc::Receiver<SocketReadChannel>,
    proxy_tx: mpsc::Sender<ProxyMessage>,
) -> Result<()> {
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

/// UDP ASSOCIATE: relay datagrams between a local UDP socket and one engine UDP socket until the
/// client closes the control connection (RFC 1928 section 7).
async fn udp_associate(
    mut stream: TcpStream,
    proxy_tx: mpsc::Sender<ProxyMessage>,
    inbound: InboundPort,
    mut client: Option<SocketAddr>,
) -> Result<()> {
    let relay = UdpSocket::bind("127.0.0.1:0").await?;
    // Held for the whole association; later associations get an ephemeral port instead.
    let inbound_claim =
        inbound.and_then(|(port, permit)| Some((port, permit.try_acquire_owned().ok()?)));

    let (resp, handle_rx) = oneshot::channel();
    let (read, mut read_rx) = mpsc::channel::<SocketReadChannel>(100);
    proxy_tx
        .send(ProxyMessage::Connect {
            msg: ConnectMessage::UDP {
                local_port: inbound_claim.as_ref().map_or(0, |(port, _)| *port),
                resp,
                read,
            },
        })
        .await?;
    let Some(handle) = handle_rx.await? else {
        reply(&mut stream, HOST_UNREACHABLE).await?;
        return Err(anyhow::anyhow!("engine could not create a UDP socket"));
    };

    let [hi, lo] = relay.local_addr()?.port().to_be_bytes();
    stream
        .write_all(&[5, SUCCEEDED, 0, 1, 127, 0, 0, 1, hi, lo])
        .await?;

    // Unless declared in the request, the first sender on the relay is taken to be the client;
    // anyone else is ignored.
    // shortcut: no TTL and no size limit; fine for one client's tracker hostnames.
    let mut resolved: HashMap<String, Ipv4Addr> = HashMap::new();
    let mut buf = vec![0u8; 65536];
    let mut control = [0u8; 64];

    loop {
        tokio::select! {
            r = stream.read(&mut control) => {
                if matches!(r, Ok(0) | Err(_)) {
                    return Ok(()); // association ends with the control connection
                }
            }
            r = relay.recv_from(&mut buf) => {
                let (n, from) = r?;
                if *client.get_or_insert(from) != from {
                    continue;
                }
                let Some((target, port, data)) = parse_udp_header(&buf[..n]) else {
                    continue; // fragmented or malformed: dropped, as RFC 1928 allows
                };
                let ip = match target {
                    Target::V4(ip) => ip,
                    Target::Domain(name) => match resolved.get(&name) {
                        Some(ip) => *ip,
                        None => match resolve(&proxy_tx, name.clone()).await? {
                            Some(ip) => *resolved.entry(name).or_insert(ip),
                            None => continue,
                        },
                    },
                    Target::V6 => continue, // the tunnel is IPv4 only
                };
                proxy_tx
                    .send(ProxyMessage::Write {
                        msg: WriteMessage::UDP {
                            handle,
                            data: bytes::Bytes::copy_from_slice(data),
                            dst: ip.octets(),
                            dst_port: port,
                        },
                    })
                    .await?;
            }
            Some(Some((data, from))) = read_rx.recv() => {
                let (Some(client), IpAddress::Ipv4(ip)) = (client, from.addr) else {
                    continue;
                };
                let mut packet = Vec::with_capacity(10 + data.len());
                packet.extend_from_slice(&[0, 0, 0, 1]);
                packet.extend_from_slice(&ip.0);
                packet.extend_from_slice(&from.port.to_be_bytes());
                packet.extend_from_slice(&data);
                relay.send_to(&packet, client).await?;
            }
        }
    }
}

/// Split a SOCKS5 UDP datagram into destination and payload. `None` if malformed or fragmented.
fn parse_udp_header(packet: &[u8]) -> Option<(Target, u16, &[u8])> {
    let (&[0, 0, frag, atyp], rest) = packet.split_first_chunk::<4>()? else {
        return None;
    };
    if frag != 0 {
        return None;
    }
    let (target, rest) = match atyp {
        1 => {
            let (ip, rest) = rest.split_first_chunk::<4>()?;
            (Target::V4(Ipv4Addr::from(*ip)), rest)
        }
        3 => {
            let (&len, rest) = rest.split_first()?;
            let (name, rest) = rest.split_at_checked(len as usize)?;
            (Target::Domain(String::from_utf8(name.to_vec()).ok()?), rest)
        }
        4 => (Target::V6, rest.get(16..)?),
        _ => return None,
    };
    let (port, data) = rest.split_first_chunk::<2>()?;
    Some((target, u16::from_be_bytes(*port), data))
}

#[cfg(test)]
mod udp_header_tests {
    use super::*;

    #[test]
    fn parses_ipv4_and_domain() {
        let packet = [0, 0, 0, 1, 8, 8, 8, 8, 0, 53, b'h', b'i'];
        let Some((Target::V4(ip), 53, b"hi")) = parse_udp_header(&packet) else {
            panic!("IPv4 datagram not parsed");
        };
        assert_eq!(ip, Ipv4Addr::new(8, 8, 8, 8));

        let mut packet = vec![0, 0, 0, 3, 3, b'a', b'.', b'b', 0x1F, 0x90];
        packet.extend_from_slice(b"x");
        let Some((Target::Domain(name), 8080, b"x")) = parse_udp_header(&packet) else {
            panic!("domain datagram not parsed");
        };
        assert_eq!(name, "a.b");
    }

    #[test]
    fn rejects_fragments_and_truncation() {
        assert!(parse_udp_header(&[0, 0, 1, 1, 8, 8, 8, 8, 0, 53]).is_none()); // FRAG != 0
        assert!(parse_udp_header(&[0, 0, 0, 1, 8, 8]).is_none()); // address cut short
        assert!(parse_udp_header(&[0, 0, 0, 3, 9, b'a']).is_none()); // name longer than packet
        assert!(parse_udp_header(&[0, 0, 0, 9, 0, 0]).is_none()); // unknown address type
    }
}
