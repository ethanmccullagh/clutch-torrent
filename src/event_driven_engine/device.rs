use smoltcp::phy::{Checksum, Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use std::collections::VecDeque;
use tokio::sync::mpsc;

/// smoltcp device backed by channels. The engine owns the tunnel receiver (so it can await it)
/// and pushes received packets into `inbox` before polling.
pub struct QueueDevice {
    pub inbox: VecDeque<Vec<u8>>,
    pub vpn_tx: mpsc::Sender<Vec<u8>>,
}

impl QueueDevice {
    pub fn new(vpn_tx: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            inbox: VecDeque::new(),
            vpn_tx,
        }
    }
}

impl Device for QueueDevice {
    type RxToken<'a>
        = ChannelRxToken
    where
        Self: 'a;

    type TxToken<'a>
        = ChannelTxToken<'a>
    where
        Self: 'a;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let buffer = self.inbox.pop_front()?;
        Some((
            ChannelRxToken { buffer },
            ChannelTxToken {
                vpn_tx: &self.vpn_tx,
            },
        ))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(ChannelTxToken {
            vpn_tx: &self.vpn_tx,
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = 1500;
        caps.medium = Medium::Ip;
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
