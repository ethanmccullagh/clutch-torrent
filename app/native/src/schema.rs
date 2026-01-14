
use smoltcp::iface::{SocketHandle};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken, Checksum};
use smoltcp::time::Instant;
use smoltcp::wire::{Ipv4Address, Ipv4Packet};
use std::net::SocketAddr;
use tokio::sync::{mpsc, oneshot};

const LOCAL_IP: Ipv4Address = Ipv4Address::new(127, 0, 0, 1);

pub enum SocketType {
    TCP,
    UDP,
    ICMP
}


pub enum ProxyMessage {
    Connect {
        addr: Option<[u8; 4]>,
        port: Option<u16>,
        resp: oneshot::Sender<Option<SocketHandle>>,
        socket_type: SocketType
    },
    Close {
        handle: SocketHandle,
        resp: oneshot::Sender<bool>,
    },
    Read {
        handle: SocketHandle,
        resp: oneshot::Sender<Option<Vec<u8>>>,
    },
    Write {
        handle: SocketHandle,
        data: Vec<u8>,
        dst: Option<[u8;4]>,
        resp: oneshot::Sender<bool>,
        send_at: Instant,
        socket_type: SocketType
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
