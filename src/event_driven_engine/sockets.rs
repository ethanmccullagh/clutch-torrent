use super::device::{QueueDevice, MTU};
use crate::schema::{ConnectMessage, SocketReadChannel, WriteMessage};
use crate::setup::{dns_server, setup_engine};
use bytes::Bytes;
use ini::Ini;
use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, Tracer};
use smoltcp::socket::tcp::State;
use smoltcp::socket::{dns, icmp, tcp, udp, Socket};
use smoltcp::time::{Duration, Instant};
use smoltcp::wire::{
    DnsQueryType, Icmpv4Packet, Icmpv4Repr, IpAddress, IpEndpoint, IpListenEndpoint, Ipv4Address,
};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::Ipv4Addr;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};
use tokio::task::AbortHandle;

// Each direction; caps the TCP window, so too small throttles every connection.
const TCP_BUFFER_SIZE: usize = 64 * 1024;
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
    /// Ephemeral port picked by the engine, freed when the socket is removed.
    ephemeral_port: Option<u16>,
    /// Outgoing TCP: the Connect reply, held until the handshake succeeds or fails.
    connecting: Option<oneshot::Sender<Option<SocketHandle>>>,
    kind: Kind,
}

/// Which smoltcp socket a handle refers to; `SocketSet::get_mut` panics on the wrong type.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Tcp,
    Udp,
    Icmp,
}

impl Connection {
    fn new(
        read: mpsc::Sender<SocketReadChannel>,
        handle: SocketHandle,
        wake: &mpsc::UnboundedSender<SocketHandle>,
        ephemeral_port: Option<u16>,
        kind: Kind,
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
            ephemeral_port,
            connecting: None,
            kind,
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
    /// Ephemeral ports in use, so two sockets never get the same one.
    used_ports: HashSet<u16>,
    /// The VPN gateway (the config's DNS server), which also answers NAT-PMP.
    gateway: Ipv4Address,
    dns: SocketHandle,
    dns_queries: Vec<(dns::QueryHandle, oneshot::Sender<Option<Ipv4Addr>>)>,
}

impl Engine {
    pub fn new(
        conf: Ini,
        vpn_sender: mpsc::UnboundedSender<Vec<u8>>,
        wake: mpsc::UnboundedSender<SocketHandle>,
    ) -> Self {
        let gateway = dns_server(&conf);
        let (iface, device, mut sockets) = setup_engine(conf, QueueDevice::new(vpn_sender));
        let dns = sockets.add(dns::Socket::new(&[IpAddress::Ipv4(gateway)], vec![]));
        Self {
            iface,
            device,
            sockets,
            connections: HashMap::new(),
            wake,
            used_ports: HashSet::new(),
            gateway,
            dns,
            dns_queries: Vec::new(),
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
            let moved = self.flush_writes()
                | self.drain_reads()
                | self.finish_queries()
                | self.finish_connects();
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
                            let from = sock
                                .remote_endpoint()
                                .unwrap_or(IpEndpoint::new(IpAddress::v4(0, 0, 0, 0), 0));
                            sock.recv_slice(&mut buf)
                                .ok()
                                .map(|n| Some((Bytes::copy_from_slice(&buf[..n]), from)))
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
                    Socket::Udp(sock) => sock
                        .recv_slice(&mut buf)
                        .ok()
                        .map(|(n, meta)| Some((Bytes::copy_from_slice(&buf[..n]), meta.endpoint))),
                    Socket::Icmp(sock) => loop {
                        match sock.recv() {
                            // Echo requests (e.g. the server's MTU probes) are answered by smoltcp
                            // itself; readers only want replies and errors.
                            Ok((data, _)) if data.first() == Some(&8) => continue,
                            Ok((data, addr)) => {
                                break Some(Some((
                                    Bytes::copy_from_slice(data),
                                    IpEndpoint::new(addr, 0),
                                )))
                            }
                            Err(_) => break None,
                        }
                    },
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
            if let Some(port) = self
                .connections
                .remove(&handle)
                .and_then(|c| c.ephemeral_port)
            {
                self.used_ports.remove(&port);
            }
        }
        moved
    }

    pub fn connect(&mut self, msg: ConnectMessage) {
        let mut wait_for_handshake = false;
        let (handle, resp, read, port, kind) = match msg {
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

                let Some(port) = self.pick_port(local_port) else {
                    eprintln!("no free ephemeral port");
                    let _ = resp.send(None);
                    return;
                };
                if let Err(e) = socket.bind(IpListenEndpoint {
                    addr: self.iface.ipv4_addr().map(IpAddress::Ipv4),
                    port,
                }) {
                    eprintln!("smoltcp UDP bind error: {}", e);
                    let _ = resp.send(None);
                    return;
                }
                (self.sockets.add(socket), resp, read, Some(port), Kind::Udp)
            }
            ConnectMessage::TCP {
                remote,
                local_port,
                connect,
                resp,
                read,
            } => {
                // The interface has no IPv6 address; smoltcp panics if asked to send from one.
                if !matches!(remote.addr, IpAddress::Ipv4(_)) {
                    eprintln!("TCP connect to {remote} refused: only IPv4 is supported");
                    let _ = resp.send(None);
                    return;
                }
                let rx_buffer = tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]);
                let tx_buffer = tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]);
                let mut socket = tcp::Socket::new(rx_buffer, tx_buffer);
                socket.set_timeout(Some(TCP_TIMEOUT));

                let Some(port) = self.pick_port(local_port) else {
                    eprintln!("no free ephemeral port");
                    let _ = resp.send(None);
                    return;
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
                wait_for_handshake = connect;
                (self.sockets.add(socket), resp, read, Some(port), Kind::Tcp)
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
                (self.sockets.add(socket), resp, read, None, Kind::Icmp)
            }
        };

        // Only engine-picked ports are tracked; callers asking for a fixed port manage it themselves.
        let ephemeral = port.filter(|p| self.used_ports.insert(*p));
        let mut conn = Connection::new(read, handle, &self.wake, ephemeral, kind);
        if wait_for_handshake {
            conn.connecting = Some(resp);
        } else {
            let _ = resp.send(Some(handle));
        }
        self.connections.insert(handle, conn);
    }

    /// Answer outgoing TCP connects whose handshake finished: the handle if it was established,
    /// `None` if it was refused or timed out.
    fn finish_connects(&mut self) -> bool {
        let mut moved = false;
        for (handle, conn) in self.connections.iter_mut() {
            if conn.connecting.is_none() {
                continue;
            }
            let result = match self.sockets.get::<tcp::Socket>(*handle).state() {
                State::SynSent | State::SynReceived => continue,
                State::Closed => None,
                // Established, or already closing after the peer sent data and a FIN.
                _ => Some(*handle),
            };
            if let Some(resp) = conn.connecting.take() {
                let _ = resp.send(result);
                moved = true;
            }
        }
        moved
    }

    /// `requested`, or a free ephemeral port if it is 0. `None` if every ephemeral port is taken.
    fn pick_port(&self, requested: u16) -> Option<u16> {
        if requested != 0 {
            return Some(requested);
        }
        if self.used_ports.len() >= EPHEMERAL_PORTS.len() {
            return None;
        }
        loop {
            let port = rand::random_range(EPHEMERAL_PORTS);
            if !self.used_ports.contains(&port) {
                return Some(port);
            }
        }
    }

    pub fn resolve(&mut self, name: &str, resp: oneshot::Sender<Option<Ipv4Addr>>) {
        // smoltcp sends .local names as mDNS, including to an IPv6 multicast address we can't send from.
        if name.trim_end_matches('.').ends_with(".local") {
            let _ = resp.send(None);
            return;
        }
        let socket = self.sockets.get_mut::<dns::Socket>(self.dns);
        match socket.start_query(self.iface.context(), name, DnsQueryType::A) {
            Ok(query) => self.dns_queries.push((query, resp)),
            Err(e) => {
                eprintln!("DNS query for {name:?} not started: {e}");
                let _ = resp.send(None);
            }
        }
    }

    /// Answer DNS queries that have finished (smoltcp retries and times them out itself).
    fn finish_queries(&mut self) -> bool {
        let socket = self.sockets.get_mut::<dns::Socket>(self.dns);
        let mut moved = false;
        for i in (0..self.dns_queries.len()).rev() {
            let (query, resp) = &self.dns_queries[i];
            let result = match socket.get_query_result(*query) {
                Err(dns::GetQueryResultError::Pending) if !resp.is_closed() => continue,
                Err(dns::GetQueryResultError::Pending) => {
                    socket.cancel_query(*query); // requester gave up
                    None
                }
                Err(dns::GetQueryResultError::Failed) => None,
                Ok(addrs) => addrs.iter().find_map(|a| match a {
                    IpAddress::Ipv4(v4) => Some(Ipv4Addr::from(v4.0)),
                    _ => None,
                }),
            };
            let (_, resp) = self.dns_queries.swap_remove(i);
            let _ = resp.send(result);
            moved = true;
        }
        moved
    }

    pub fn write(&mut self, msg: WriteMessage) {
        let (handle, kind) = match &msg {
            WriteMessage::TCP { handle, .. } => (*handle, Kind::Tcp),
            WriteMessage::UDP { handle, .. } | WriteMessage::NAT { handle, .. } => {
                (*handle, Kind::Udp)
            }
            WriteMessage::ICMP { handle, .. } => (*handle, Kind::Icmp),
        };
        // SocketSet::get_mut panics on a removed handle or the wrong socket type.
        let Some(conn) = self.connections.get_mut(&handle) else {
            eprintln!("Write to unknown socket {}, dropped", handle);
            return;
        };
        if conn.kind != kind {
            eprintln!("{kind:?} write to {:?} socket {handle}, dropped", conn.kind);
            return;
        }

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
                // Bound to the first write's ident; replies to other idents won't reach this socket.
                if !socket.is_open() {
                    if let Err(e) = socket.bind(icmp::Endpoint::Ident(ident)) {
                        eprintln!("ICMP socket {handle} bind to ident {ident} failed: {e}");
                        return;
                    }
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
                let remote = IpEndpoint::new(IpAddress::Ipv4(self.gateway), 5351);
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

const EPHEMERAL_PORTS: std::ops::RangeInclusive<u16> = 49152..=65535;
