use crate::schema::{VpnEngine, ChannelMessage};
use anyhow::Result;
use boringtun::noise::TunnResult;
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use smoltcp::time::Instant;
use smoltcp::iface::{SocketHandle};
use smoltcp::socket::tcp;
use std::net::SocketAddr;

// --- CONFIGURATION (Replace these with your ProtonVPN details) ---
impl<'a> VpnEngine<'a> {
    pub fn process_packets(&mut self) {
        let now = Instant::now();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
    }

    pub fn add_sock(&mut self, socket: tcp::Socket<'a>) -> SocketHandle {
        self.sockets.add(socket)
    }

    pub fn get_sock(&mut self, handle: &SocketHandle) -> &tcp::Socket<'a> {
        self.sockets.get_mut(*handle)
    }

    pub fn send_to_vpn(&mut self) {
        if let Ok(ip_packet) = self.device_tx_receiver.try_recv() {
            let mut buff = [0u8; 4096];

            match self.tunn.encapsulate(&ip_packet, &mut buff) {
                TunnResult::WriteToNetwork(encrypted_bytes) => {
                    let _ = self.udp_send(encrypted_bytes);
                }
                TunnResult::Err(e) => println!("Tunnel encap error {:?}", e),
                _ => {}
            }
        }
    }

    pub fn recv_from_vpn(&mut self) {
        let mut dst_buf = [0u8; 4096];
        let mut decrypted_buf = [0u8; 4096];

        if let Ok((len, addr)) = self.udp_recv(&mut dst_buf) {
            if addr != self.endpoint {
                return;
            }

            let mut result = self.decapsulate(&mut dst_buf[..len], &mut decrypted_buf);
            loop {
                match result {
                    TunnResult::Done => return,
                    TunnResult::WriteToNetwork(b) => {
                        let _ = self.udp_send(b);
                        result = self.decapsulate(&[], &mut decrypted_buf);
                    }
                    TunnResult::WriteToTunnelV4(b, _) => {
                        let _ = self.device_rx_sender.send(b.to_vec());
                        result = self.decapsulate(&[], &mut decrypted_buf);
                    }
                    TunnResult::WriteToTunnelV6(b, _) => {
                        let _ = self.device_rx_sender.send(b.to_vec());
                        result = self.decapsulate(&[], &mut decrypted_buf);
                    }
                    _ => return,
                }
            }
        }
    }

    fn decapsulate<'b>(
        &mut self,
        src_buff: &[u8],
        dst_buf: &'b mut [u8; 4096],
    ) -> TunnResult<'b> {
        self.tunn.decapsulate(None, src_buff, dst_buf)
    }

    fn udp_send(
        &mut self,
        encrypted_bytes: &mut [u8],
    ) -> Result<(), TrySendError<ChannelMessage>> {
        match self.udp_send_channel.try_send(ChannelMessage {
            data: (encrypted_bytes.try_into().unwrap()),
            addr: (self.endpoint),
        }) {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        }
    }
    fn udp_recv(
        &mut self,
        recv_buf: &mut [u8; 4096],
    ) -> Result<(usize, SocketAddr), TryRecvError> {
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
