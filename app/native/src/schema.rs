use boringtun::noise::{Tunn, TunnResult};
use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities, Medium};
use smoltcp::time::Instant;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::error::Error;
use std::io;
use std::net::SocketAddr;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
pub enum ProxyMessage {
    Connect {
        addr: [u8; 4],
        port: u16,
        resp: mpsc::Sender<bool>,
    },
    Data {
        data: Vec<u8>,
    },
    Close,
}

pub struct ChannelMessage {
    pub data: Vec<u8>,
    pub addr: SocketAddr,
}

pub struct VpnEngine {
    pub iface: Interface,
    pub sockets: SocketSet<'static>,
    pub device: MyVirtualDevice,
    pub tunn: Tunn,
    pub connection_map: HashMap<usize, SocketHandle>,
    pub endpoint: SocketAddr,
    pub udp_send_channel: mpsc::Sender<ChannelMessage>,
    pub udp_recv_channel: mpsc::Receiver<ChannelMessage>,
}

impl VpnEngine {
    pub async fn encapsulate<'a>(
        &mut self,
        ip_packet: &Vec<u8>,
        scratch_buf: &'a mut [u8; 4096],
    ) -> TunnResult<'a> {
        self.tunn.encapsulate(ip_packet, scratch_buf)
    }

    pub async fn decapsulate<'a>(
        &mut self,
        packet: &[u8],
        scratch_buf: &'a mut [u8; 4096],
    ) -> TunnResult<'a> {
        self.tunn.decapsulate(None, packet, scratch_buf)
    }

    pub async fn process_packets(&mut self) {
        let now = Instant::now();
        // Here we pass references to the internal fields mutably
        self.iface.poll(now, &mut self.device, &mut self.sockets);
    }

    pub fn poll_delay(&mut self, now: Instant) -> Option<smoltcp::time::Duration> {
        self.iface.poll_delay(now, &self.sockets)
    }

    pub async fn tx_push_back(&mut self, ip_packet: Vec<u8>) {
        self.device.tx_queue.push_back(ip_packet);
    }

    pub async fn rx_push_back(&mut self, ip_packet: Vec<u8>) {
        self.device.rx_queue.push_back(ip_packet);
    }

    pub async fn tx_pop_front(&mut self) -> Option<Vec<u8>> {
        self.device.tx_queue.pop_front()
    }

    pub async fn rx_pop_front(&mut self) -> Option<Vec<u8>> {
        self.device.rx_queue.pop_front()
    }

    pub async fn udp_send(
        &mut self,
        encrypted_bytes: &mut [u8],
    ) -> Result<(), mpsc::error::SendError<ChannelMessage>> {
        match self
            .udp_send_channel
            .send(ChannelMessage {
                data: (encrypted_bytes.try_into().unwrap()),
                addr: (self.endpoint),
            })
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        }
    }
    pub async fn udp_recv(
        &mut self,
        recv_buf: &mut [u8;4096],
    ) -> Result<(usize,SocketAddr), mpsc::error::TryRecvError> {
        match self.udp_recv_channel.try_recv() {
            Ok(msg) => {
                let len = msg.data.len().min(4096);
                recv_buf[..len].copy_from_slice(&msg.data[..len]);
                Ok((len, msg.addr))
            }
            Err(e) => Err(e),
        }
    }
}

pub struct MyVirtualDevice {
    pub rx_queue: VecDeque<Vec<u8>>,
    pub tx_queue: VecDeque<Vec<u8>>,
}

impl Device for MyVirtualDevice {
    type RxToken<'a>
        = RxToken
    where
        Self: 'a;

    type TxToken<'a>
        = TxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        if self.rx_queue.is_empty() {
            return None;
        }
        let buffer: Vec<u8> = self.rx_queue.pop_front().unwrap();
        Some((RxToken(buffer), TxToken(self)))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(TxToken(self))
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps: DeviceCapabilities = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ip; // We work at the IP layer, not Ethernet
        caps
    }
}

// Tokens required by smoltcp to manage buffers
pub struct RxToken(Vec<u8>);
impl smoltcp::phy::RxToken for RxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.0)
    }
}

pub struct TxToken<'a>(&'a mut MyVirtualDevice);
impl<'a> smoltcp::phy::TxToken for TxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer: Vec<u8> = vec![0u8; len];
        let result: R = f(&mut buffer);
        self.0.tx_queue.push_back(buffer);
        result
    }
}
