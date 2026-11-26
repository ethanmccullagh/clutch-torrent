//! Incoming connections: maps a port on the VPN server with NAT-PMP, keeps the mapping alive, and
//! forwards incoming TCP connections to the client's listen port on 127.0.0.1. Incoming UDP is
//! delivered through the SOCKS5 UDP association bound to the same port (see `socks5.rs`).
//!
//! The tunnel-side port is the client's own listen port number, so one number configures both.

use crate::schema::{ConnectMessage, ProxyMessage, SocketReadChannel, WriteMessage};
use crate::socks5::relay;
use anyhow::{bail, Context, Result};
use bytes::Bytes;
use smoltcp::iface::SocketHandle;
use smoltcp::wire::{IpAddress, IpEndpoint};
use std::net::Ipv4Addr;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};

/// Listening sockets kept ready; each accepts one connection, then is replaced.
const LISTENERS: usize = 8;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Mapping {
    pub public_ip: Ipv4Addr,
    pub tcp_port: u16,
    pub udp_port: u16,
    /// Seconds the server keeps the mapping (Proton grants 60).
    pub lifetime: u32,
}

/// Map `port` (TCP and UDP) on the VPN server and forward incoming TCP connections to
/// 127.0.0.1:`port`. Returns the first mapping; renewal and accepting continue in the background
/// until the engine stops. Peers must be told the mapping's public ports, not `port`.
pub async fn start_port_forward(
    proxy_tx: mpsc::Sender<ProxyMessage>,
    port: u16,
) -> Result<Mapping> {
    let mut nat = NatPmp::new(proxy_tx.clone()).await?;
    let mapping = nat.map(port).await?;

    tokio::spawn(async move {
        let mut current = mapping;
        loop {
            if nat.proxy_tx.is_closed() {
                return; // engine stopped
            }
            // RFC 6886: renew at half the lifetime.
            tokio::time::sleep(Duration::from_secs((current.lifetime / 2).max(1).into())).await;
            match nat.map(port).await {
                Ok(renewed) => {
                    if (renewed.public_ip, renewed.tcp_port, renewed.udp_port)
                        != (current.public_ip, current.tcp_port, current.udp_port)
                    {
                        println!(
                            "Port mapping changed, update the client's announced port: {renewed}"
                        );
                    }
                    current = renewed;
                }
                Err(e) => {
                    eprintln!("NAT-PMP renewal failed, retrying: {e:#}");
                    current.lifetime = 10; // retry in 5 s
                }
            }
        }
    });

    for _ in 0..LISTENERS {
        tokio::spawn(accept_loop(proxy_tx.clone(), port));
    }
    Ok(mapping)
}

impl std::fmt::Display for Mapping {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TCP {ip}:{} UDP {ip}:{} (lifetime {} s)",
            self.tcp_port,
            self.udp_port,
            self.lifetime,
            ip = self.public_ip
        )
    }
}

/// Keep one listening socket on `port`; forward each accepted connection to the client.
async fn accept_loop(proxy_tx: mpsc::Sender<ProxyMessage>, port: u16) {
    loop {
        let (resp, accepted) = oneshot::channel();
        let (read, read_rx) = mpsc::channel::<SocketReadChannel>(100);
        let listen = ProxyMessage::Connect {
            msg: ConnectMessage::TCP {
                remote: IpEndpoint::new(IpAddress::v4(0, 0, 0, 0), 0),
                local_port: port,
                connect: false,
                resp,
                read,
            },
        };
        if proxy_tx.send(listen).await.is_err() {
            return; // engine stopped
        }
        let handle = match accepted.await {
            Ok(Some(handle)) => handle,
            Ok(None) => {
                // Listen failed, or the peer aborted mid-handshake; don't spin if it keeps failing.
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            Err(_) => return, // engine stopped
        };
        let proxy_tx = proxy_tx.clone();
        tokio::spawn(async move {
            match TcpStream::connect(("127.0.0.1", port)).await {
                Ok(stream) => {
                    if let Err(e) = relay(stream, handle, read_rx, proxy_tx).await {
                        eprintln!("incoming connection on port {port}: {e}");
                    }
                }
                // Dropping read_rx closes the engine socket, so the peer sees the connection end.
                Err(e) => eprintln!(
                    "incoming connection dropped, nothing listening on 127.0.0.1:{port}: {e}"
                ),
            }
        });
    }
}

/// NAT-PMP (RFC 6886) client talking to the gateway from an engine UDP socket.
struct NatPmp {
    proxy_tx: mpsc::Sender<ProxyMessage>,
    handle: SocketHandle,
    replies: mpsc::Receiver<SocketReadChannel>,
}

impl NatPmp {
    async fn new(proxy_tx: mpsc::Sender<ProxyMessage>) -> Result<Self> {
        let (resp, handle) = oneshot::channel();
        let (read, replies) = mpsc::channel(10);
        proxy_tx
            .send(ProxyMessage::Connect {
                msg: ConnectMessage::UDP {
                    local_port: 0,
                    resp,
                    read,
                },
            })
            .await?;
        let handle = handle
            .await?
            .context("engine could not create the NAT-PMP socket")?;
        Ok(Self {
            proxy_tx,
            handle,
            replies,
        })
    }

    /// Request (or renew) TCP and UDP mappings for `port`.
    async fn map(&mut self, port: u16) -> Result<Mapping> {
        let public = self.request(0, 0).await?;
        let udp = self.request(1, port).await?;
        let tcp = self.request(2, port).await?;
        Ok(Mapping {
            public_ip: Ipv4Addr::new(public[8], public[9], public[10], public[11]),
            udp_port: u16::from_be_bytes([udp[10], udp[11]]),
            tcp_port: u16::from_be_bytes([tcp[10], tcp[11]]),
            lifetime: u32::from_be_bytes([tcp[12], tcp[13], tcp[14], tcp[15]])
                .min(u32::from_be_bytes([udp[12], udp[13], udp[14], udp[15]])),
        })
    }

    /// Send one request; return its reply (checked: opcode, result code, length). UDP can lose
    /// packets, so it tries 3 times.
    async fn request(&mut self, opcode: u8, internal_port: u16) -> Result<Bytes> {
        let reply_len = if opcode == 0 { 12 } else { 16 };
        for _ in 0..3 {
            self.proxy_tx
                .send(ProxyMessage::Write {
                    msg: WriteMessage::NAT {
                        handle: self.handle,
                        opcode,
                        internal_port,
                    },
                })
                .await?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while let Ok(reply) = tokio::time::timeout_at(deadline, self.replies.recv()).await {
                let Some(Some((reply, _))) = reply else {
                    bail!("NAT-PMP socket closed");
                };
                if reply.len() < reply_len || reply[1] != 128 + opcode {
                    continue; // a late reply to an earlier request
                }
                let code = u16::from_be_bytes([reply[2], reply[3]]);
                if code != 0 {
                    bail!("NAT-PMP opcode {opcode} refused with result code {code}");
                }
                return Ok(reply);
            }
        }
        bail!("no NAT-PMP reply from the gateway for opcode {opcode}")
    }
}
