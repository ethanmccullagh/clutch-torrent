use bytes::Bytes;
use smoltcp::iface::SocketHandle;
use smoltcp::wire::IpEndpoint;
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};

/// `Some((data, from))` is received data; `from` is the sender (port 0 for ICMP). `None` is EOF:
/// the TCP connection closed or failed.
pub type SocketReadChannel = Option<(Bytes, IpEndpoint)>;

pub enum WriteMessage {
    ICMP {
        handle: SocketHandle,
        dst: [u8; 4],
        ident: u16,
        seq: u16,
    },
    TCP {
        handle: SocketHandle,
        data: Bytes,
    },
    UDP {
        handle: SocketHandle,
        data: Bytes,
        dst: [u8; 4],
        dst_port: u16,
    },
    NAT {
        handle: SocketHandle,
        opcode: u8,
    },
}

pub enum ConnectMessage {
    ICMP {
        resp: oneshot::Sender<Option<SocketHandle>>,
        read: mpsc::Sender<SocketReadChannel>,
    },
    TCP {
        remote: IpEndpoint,
        local_port: u16,
        connect: bool,
        resp: oneshot::Sender<Option<SocketHandle>>,
        read: mpsc::Sender<SocketReadChannel>,
    },
    UDP {
        local_port: u16,
        resp: oneshot::Sender<Option<SocketHandle>>,
        read: mpsc::Sender<SocketReadChannel>,
    },
}

pub enum ProxyMessage {
    Connect {
        msg: ConnectMessage,
    },
    /// Look up a hostname's IPv4 address through the tunnel. `None` if it doesn't resolve.
    Resolve {
        name: String,
        resp: oneshot::Sender<Option<std::net::Ipv4Addr>>,
    },
    Write {
        msg: WriteMessage,
    },
    Close,
}

pub struct ChannelMessage {
    pub data: Vec<u8>,
    pub addr: SocketAddr,
}
