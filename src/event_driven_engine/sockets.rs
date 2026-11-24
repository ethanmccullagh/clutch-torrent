use super::device::{QueueDevice, MTU};
use crate::schema::{ConnectMessage, SocketReadChannel, WriteMessage};
use crate::setup::setup_engine;
use bytes::Bytes;
use ini::Ini;
use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, Tracer};
use smoltcp::socket::tcp::State;
use smoltcp::socket::{icmp, tcp, udp, Socket};
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpAddress, IpEndpoint, IpListenEndpoint};
use std::collections::{HashMap, VecDeque};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::AbortHandle;

const TCP_BUFFER_SIZE: usize = 256;
// UDP/ICMP buffers must hold one full-MTU packet; smaller ones silently drop bigger datagrams.
const UDP_BUFFER_SIZE: usize = MTU;
const ICMP_BUFFER_SIZE: usize = MTU;
const READ_CHUNK: usize = 2048;
// Without a timeout, a closing socket whose peer vanished retransmits its FIN forever and is never pruned.
const TCP_TIMEOUT: Duration = Duration::from_secs(60);

/// Engine-side state for one socket.
struct Connection {
    read: mpsc::Sender<SocketReadChannel>,
    /// TCP data waiting for room in the smoltcp send buffer.
    pending: VecDeque<Bytes>,
    /// Set while `read` is full; the task wakes the engine once the reader makes room.
    blocked: Option<AbortHandle>,
    /// Wakes the engine when the reader drops its end, so the socket gets closed.
    closed_watch: AbortHandle,
    eof_sent: bool,
}

impl Connection {
    fn new(
        read: mpsc::Sender<SocketReadChannel>,
        handle: SocketHandle,
        wake: &mpsc::UnboundedSender<SocketHandle>,
    ) -> Self {
        let (watch_read, wake) = (read.clone(), wake.clone());
        let closed_watch = tokio::spawn(async move {
            watch_read.closed().await;
            let _ = wake.send(handle);
        })
        .abort_handle();
        Self {
            read,
            pending: VecDeque::new(),
            blocked: None,
            closed_watch,
            eof_sent: false,
        }
    }

    fn block(&mut self, handle: SocketHandle, wake: &mpsc::UnboundedSender<SocketHandle>) {
        let (read, wake) = (self.read.clone(), wake.clone());
        self.blocked = Some(
            tokio::spawn(async move {
                let _ = read.reserve().await; // permit dropped at once: only waiting for room
                let _ = wake.send(handle);
            })
            .abort_handle(),
        );
    }
}

impl Drop for Connection {
    // The watcher tasks hold Sender clones; abort them so readers see the channel end.
    fn drop(&mut self) {
        self.closed_watch.abort();
        if let Some(task) = &self.blocked {
            task.abort();
        }
    }
}

pub struct Engine {
    iface: Interface,
    device: Tracer<QueueDevice>,
    sockets: SocketSet<'static>,
    connections: HashMap<SocketHandle, Connection>,
    wake: mpsc::UnboundedSender<SocketHandle>,
}

impl Engine {
    pub fn new(
        conf: Ini,
        vpn_sender: mpsc::Sender<Vec<u8>>,
        wake: mpsc::UnboundedSender<SocketHandle>,
    ) -> Self {
        let (iface, device, sockets) = setup_engine(conf, QueueDevice::new(vpn_sender));
        Self {
            iface,
            device,
            sockets,
            connections: HashMap::new(),
            wake,
        }
    }

    pub fn push_packet(&mut self, packet: Vec<u8>) {
        self.device.get_mut().inbox.push_back(packet);
    }

    pub fn poll_delay(&mut self) -> Option<Duration> {
        self.iface.poll_delay(Instant::now(), &self.sockets)
    }

    /// Called when a reader made room in, or dropped, its channel.
    pub fn unblock(&mut self, handle: SocketHandle) {
        if let Some(task) = self
            .connections
            .get_mut(&handle)
            .and_then(|c| c.blocked.take())
        {
            task.abort();
        }
    }

    /// Move data between smoltcp and the channels until nothing more can move.
    pub fn service(&mut self) {
        loop {
            // `|` not `||`: both must run every pass.
            let moved = self.flush_writes() | self.drain_reads();
            let changed = self
                .iface
                .poll(Instant::now(), &mut self.device, &mut self.sockets);
            if !moved && !changed {
                break;
            }
        }
    }

    /// Push queued TCP writes into socket send buffers.
    fn flush_writes(&mut self) -> bool {
        let mut moved = false;
        for (handle, conn) in self.connections.iter_mut() {
            if conn.pending.is_empty() {
                continue;
            }
            let sock = self.sockets.get_mut::<tcp::Socket>(*handle);
            if !sock.is_open() {
                // Connection failed or was reset; the reader gets EOF from drain_reads.
                conn.pending.clear();
                continue;
            }
            while let Some(front) = conn.pending.front_mut() {
                match sock.send_slice(front) {
                    Ok(0) | Err(_) => break, // buffer full, or not established yet
                    Ok(n) if n == front.len() => {
                        conn.pending.pop_front();
                        moved = true;
                    }
                    Ok(n) => {
                        let _ = front.split_to(n);
                        moved = true;
                    }
                }
            }
        }
        moved
    }

    /// Forward received data to readers, send EOFs, and close sockets whose reader is gone.
    fn drain_reads(&mut self) -> bool {
        let mut moved = false;
        let mut dead = Vec::new();
        let mut buf = [0u8; READ_CHUNK];

        for (handle, socket) in self.sockets.iter_mut() {
            let Some(conn) = self.connections.get_mut(&handle) else {
                continue;
            };

            if conn.read.is_closed() {
                match socket {
                    // Graceful FIN; the socket is removed once smoltcp finishes closing it.
                    Socket::Tcp(sock) => {
                        sock.close();
                        if !sock.is_open() {
                            dead.push(handle);
                        }
                    }
                    _ => dead.push(handle),
                }
                continue;
            }
            if conn.blocked.is_some() {
                continue;
            }

            let mut full = false;
            loop {
                // Reserve before reading so a full channel never loses data already taken from smoltcp.
                let permit = match conn.read.try_reserve() {
                    Ok(permit) => permit,
                    Err(e) => {
                        full = matches!(e, TrySendError::Full(()));
                        break;
                    }
                };
                let item = match socket {
                    Socket::Tcp(sock) => {
                        if sock.can_recv() {
                            sock.recv_slice(&mut buf).ok().map(|n| {
                                Some((Bytes::copy_from_slice(&buf[..n]), IpAddress::v4(0, 0, 0, 0)))
                            })
                        } else if !conn.eof_sent
                            && !sock.may_recv()
                            && !matches!(
                                sock.state(),
                                State::Listen | State::SynSent | State::SynReceived
                            )
                        {
                            conn.eof_sent = true;
                            Some(None)
                        } else {
                            None
                        }
                    }
                    Socket::Udp(sock) => sock.recv_slice(&mut buf).ok().map(|(n, meta)| {
                        Some((Bytes::copy_from_slice(&buf[..n]), meta.endpoint.addr))
                    }),
                    Socket::Icmp(sock) => sock
                        .recv()
                        .ok()
                        .map(|(data, addr)| Some((Bytes::copy_from_slice(data), addr))),
                    _ => None,
                };
                match item {
                    Some(item) => {
                        permit.send(item);
                        moved = true;
                    }
                    None => break,
                }
            }
            if full {
                conn.block(handle, &self.wake);
            }
        }

        for handle in dead {
            eprintln!("Socket {} reader gone, pruned", handle);
            self.sockets.remove(handle);
            self.connections.remove(&handle);
        }
        moved
    }

    pub fn connect(&mut self, msg: ConnectMessage) {
        let (handle, resp, read) = match msg {
            ConnectMessage::UDP {
                local_port,
                resp,
                read,
            } => {
                let rx_buffer = udp::PacketBuffer::new(
                    vec![udp::PacketMetadata::EMPTY],
                    vec![0; UDP_BUFFER_SIZE],
                );
                let tx_buffer = udp::PacketBuffer::new(
                    vec![udp::PacketMetadata::EMPTY],
                    vec![0; UDP_BUFFER_SIZE],
                );
                let mut socket = udp::Socket::new(rx_buffer, tx_buffer);

                let port = if local_port == 0 {
                    get_ephemeral_port()
                } else {
                    local_port
                };
                if let Err(e) = socket.bind(IpListenEndpoint {
                    addr: Some(IpAddress::v4(10, 2, 0, 2)),
                    port,
                }) {
                    eprintln!("smoltcp UDP bind error: {}", e);
                    let _ = resp.send(None);
                    return;
                }
                (self.sockets.add(socket), resp, read)
            }
            ConnectMessage::TCP {
                remote,
                local_port,
                connect,
                resp,
                read,
            } => {
                let rx_buffer = tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]);
                let tx_buffer = tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]);
                let mut socket = tcp::Socket::new(rx_buffer, tx_buffer);
                socket.set_timeout(Some(TCP_TIMEOUT));

                let port = if local_port == 0 {
                    get_ephemeral_port()
                } else {
                    local_port
                };
                let result = if connect {
                    socket
                        .connect(self.iface.context(), remote, port)
                        .map_err(|e| format!("connect error: {}", e))
                } else {
                    socket
                        .listen(port)
                        .map_err(|e| format!("listen error: {}", e))
                };
                if let Err(e) = result {
                    eprintln!("smoltcp TCP {}", e);
                    let _ = resp.send(None);
                    return;
                }
                (self.sockets.add(socket), resp, read)
            }
            ConnectMessage::ICMP { resp, read } => {
                let rx_buffer = icmp::PacketBuffer::new(
                    vec![icmp::PacketMetadata::EMPTY],
                    vec![0; ICMP_BUFFER_SIZE],
                );
                let tx_buffer = icmp::PacketBuffer::new(
                    vec![icmp::PacketMetadata::EMPTY],
                    vec![0; ICMP_BUFFER_SIZE],
                );
                let socket = icmp::Socket::new(rx_buffer, tx_buffer);
                (self.sockets.add(socket), resp, read)
            }
        };

        self.connections
            .insert(handle, Connection::new(read, handle, &self.wake));
        let _ = resp.send(Some(handle));
    }

    pub fn write(&mut self, msg: WriteMessage) {
        let handle = match &msg {
            WriteMessage::TCP { handle, .. }
            | WriteMessage::UDP { handle, .. }
            | WriteMessage::ICMP { handle, .. }
            | WriteMessage::NAT { handle, .. } => *handle,
        };
        // SocketSet::get_mut panics on a removed handle.
        let Some(conn) = self.connections.get_mut(&handle) else {
            eprintln!("Write to unknown socket {}, dropped", handle);
            return;
        };

        match msg {
            WriteMessage::TCP { data, .. } => conn.pending.push_back(data),
            WriteMessage::UDP {
                data,
                dst,
                dst_port,
                ..
            } => {
                let socket = self.sockets.get_mut::<udp::Socket>(handle);
                let remote =
                    IpEndpoint::new(IpAddress::v4(dst[0], dst[1], dst[2], dst[3]), dst_port);
                if let Err(e) = socket.send_slice(&data, remote) {
                    eprintln!("UDP socket {} send error: {}", handle, e);
                }
            }
            WriteMessage::ICMP {
                dst, ident, seq, ..
            } => {
                let checksum = self.device.capabilities().checksum;
                let socket = self.sockets.get_mut::<icmp::Socket>(handle);
                if !socket.is_open() {
                    socket
                        .bind(icmp::Endpoint::Ident(0x1234))
                        .expect("binding a closed ICMP socket to a nonzero ident cannot fail");
                }
                let repr = Icmpv4Repr::EchoRequest {
                    ident,
                    seq_no: seq,
                    data: &[],
                };
                let remote = IpAddress::v4(dst[0], dst[1], dst[2], dst[3]);
                match socket.send(repr.buffer_len(), remote) {
                    Ok(payload) => repr.emit(&mut Icmpv4Packet::new_unchecked(payload), &checksum),
                    Err(e) => eprintln!("ICMP socket {} send error: {}", handle, e),
                }
            }
            WriteMessage::NAT { opcode, .. } => {
                let socket = self.sockets.get_mut::<udp::Socket>(handle);
                let remote = IpEndpoint::new(IpAddress::v4(10, 2, 0, 1), 5351);
                let port: [u8; 2] = socket.endpoint().port.to_be_bytes();
                let data = [
                    //V  Op      Reserved  IntPort           ExtPort   Lifetime
                    0x0, opcode, 0x0, 0x0, port[0], port[1], 0x0, 0x0, 0x0, 0x0, 0x0E, 0x10,
                ];
                if let Err(e) = socket.send_slice(&data, remote) {
                    eprintln!("UDP socket {} send error: {}", handle, e);
                }
            }
        }
    }
}

fn get_ephemeral_port() -> u16 {
    49152 + (rand::random::<u16>() % 16384)
}
