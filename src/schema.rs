use bytes::Bytes;
use smoltcp::iface::SocketHandle;
use smoltcp::phy::{Checksum, Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use smoltcp::wire::{IpAddress, IpEndpoint};
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};

pub type SocketReadChannel = (Bytes, IpAddress);

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
    Connect { msg: ConnectMessage },
    Write { msg: WriteMessage },
    Close,
}

pub struct ChannelMessage {
    pub data: Vec<u8>,
    pub addr: SocketAddr,
}

pub struct MyVirtualDevice {
    pub rx_queue: mpsc::Receiver<Vec<u8>>,
    pub vpn_tx: mpsc::Sender<Vec<u8>>,
}

impl Device for MyVirtualDevice {
    type RxToken<'a>
        = ChannelRxToken
    where
        Self: 'a;

    type TxToken<'a>
        = ChannelTxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        match self.rx_queue.try_recv() {
            Ok(buffer) => Some((
                ChannelRxToken { buffer },
                ChannelTxToken {
                    vpn_tx: &self.vpn_tx,
                },
            )),
            Err(_) => None,
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(ChannelTxToken {
            vpn_tx: &self.vpn_tx,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps: DeviceCapabilities = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ip; // We work at the IP layer, not Ethernet
        caps.checksum.icmpv4 = Checksum::Tx;
        caps
    }
}

// Tokens required by smoltcp to manage buffers
pub struct ChannelRxToken {
    buffer: Vec<u8>,
}

impl RxToken for ChannelRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        //println!("ENGINE: packet received");
        f(&mut self.buffer)
    }
}

pub struct ChannelTxToken<'a> {
    vpn_tx: &'a mpsc::Sender<Vec<u8>>,
}
impl<'a> TxToken for ChannelTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        //println!("ENGINE: TRANSMIT");
        let mut buffer: Vec<u8> = vec![0u8; len];
        let result: R = f(&mut buffer);

        if let Err(e) = self.vpn_tx.try_send(buffer) {
            eprintln!("ENGINE VPN TX CHANNEL ERROR {}", e);
        }

        result
    }
}
