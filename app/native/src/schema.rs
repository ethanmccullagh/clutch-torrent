use anyhow::Result;
use boringtun::noise::{Tunn, TunnResult};
use etherparse::err::ip;
use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::socket::tcp;
use smoltcp::time::Instant;
use std::collections::HashMap;
use std::net::SocketAddr;
use tokio::sync::mpsc;


pub enum ProxyMessage {
    Connect {
        addr: [u8; 4],
        port: u16,
        resp: mpsc::Sender<bool>,
    },
    Close {
        addr: [u8; 4],
        port: u16,
        resp: mpsc::Sender<bool>,
    },
}

pub struct ChannelMessage {
    pub data: Vec<u8>,
    pub addr: SocketAddr,
}

pub struct VpnEngine<'a> {
    pub iface: Interface,
    pub sockets: SocketSet<'a>,
    pub device: MyVirtualDevice,
    pub tunn: Tunn,
    pub connection_map: HashMap<usize, SocketHandle>,
    pub endpoint: SocketAddr,
    pub udp_send_channel: mpsc::Sender<ChannelMessage>,
    pub udp_recv_channel: mpsc::Receiver<ChannelMessage>,
    pub device_rx_sender: mpsc::Sender<Vec<u8>>,
    pub device_tx_receiver: mpsc::Receiver<Vec<u8>>,
    pub control_receiver: mpsc::Receiver<ProxyMessage>
}


pub struct MyVirtualDevice {
    pub rx_queue: mpsc::Receiver<Vec<u8>>,
    pub tx_queue: mpsc::Sender<Vec<u8>>,
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
                    tx_queue: &self.tx_queue,
                },
            )),
            Err(_) => None,
        }
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(ChannelTxToken {
            tx_queue: &self.tx_queue,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps: DeviceCapabilities = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ip; // We work at the IP layer, not Ethernet
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
        f(&mut self.buffer)
    }
}

pub struct ChannelTxToken<'a> {
    tx_queue: &'a mpsc::Sender<Vec<u8>>,
}
impl<'a> TxToken for ChannelTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer: Vec<u8> = vec![0u8; len];
        let result: R = f(&mut buffer);
        if let Err(e) = self.tx_queue.try_send(buffer) {
            eprintln!("TX CHANNEL FULL {}", e);
        }
        result
    }
}
