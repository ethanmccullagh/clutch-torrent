use bytes::Bytes;
use smoltcp::iface::SocketHandle;
use smoltcp::phy::{Checksum, Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use smoltcp::wire::{IpAddress, Ipv4Address, Ipv4Packet};
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};

const LOCAL_IP: Ipv4Address = Ipv4Address::new(127, 0, 0, 1);

pub type SocketReadChannel = (Bytes, IpAddress);

pub enum WriteMessage {
    ICMP {
        handle: SocketHandle,
        dst: [u8; 4],
        ident: u16,
        seq: u16,
    },
    TCP {},
    UDP {},
    RAW {
        data: Vec<u8>,
        resp: oneshot::Sender<bool>,
    },
}

pub enum ConnectMessage {
    ICMP {
        resp: oneshot::Sender<Option<SocketHandle>>,
        read: mpsc::Sender<SocketReadChannel>,
    },
    TCP {
        addr: [u8; 4],
        port: i32,
        connect: bool,
        resp: oneshot::Sender<Option<SocketHandle>>,
    },
}

pub enum ProxyMessage {
    Connect {
        msg: ConnectMessage,
    },
    Close {
        handle: SocketHandle,
        resp: oneshot::Sender<bool>,
    },
    Write {
        msg: WriteMessage,
    },
}

pub struct ChannelMessage {
    pub data: Vec<u8>,
    pub addr: SocketAddr,
}

pub struct MyVirtualDevice {
    pub rx_queue: mpsc::Receiver<Vec<u8>>,
    pub vpn_tx: mpsc::Sender<Vec<u8>>,
    pub local_tx: mpsc::Sender<Vec<u8>>,
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
                    local_tx: &self.local_tx,
                },
            )),
            Err(_) => None,
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(ChannelTxToken {
            vpn_tx: &self.vpn_tx,
            local_tx: &self.local_tx,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps: DeviceCapabilities = DeviceCapabilities::default();
        caps.max_transmission_unit = 1280;
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
    local_tx: &'a mpsc::Sender<Vec<u8>>,
}
impl<'a> TxToken for ChannelTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        //println!("ENGINE: TRANSMIT");
        let mut buffer: Vec<u8> = vec![0u8; len];
        let result: R = f(&mut buffer);

        if let Ok(packet) = Ipv4Packet::new_checked(&buffer) {
            let dst = packet.dst_addr();

            if dst.is_loopback() || dst == LOCAL_IP {
                if let Err(e) = self.local_tx.try_send(buffer) {
                    eprintln!("ENGINE LOCAL TX CHANNEL FULL {}", e);
                }
            } else {
                if let Err(e) = self.vpn_tx.try_send(buffer) {
                    eprintln!("ENGINE VPN TX CHANNEL FULL {}", e);
                }
            }
        }

        result
    }
}
