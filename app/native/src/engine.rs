use crate::schema::{ChannelMessage, ProxyMessage, VpnEngine};
use anyhow::Result;
use boringtun::noise::TunnResult;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::{tcp, Socket};
use smoltcp::time::Instant;
use smoltcp::wire::IpAddress;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::net::SocketAddr;
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};

// --- CONFIGURATION (Replace these with your ProtonVPN details) ---
impl<'a> VpnEngine<'a> {
    pub fn control_recv(&mut self) {
        if let Ok(msg) = self.control_receiver.try_recv() {
            eprintln!("Channel try receive SUCCESS");
            match msg {
                ProxyMessage::Connect { addr, port, resp } => {
                    eprintln!("Channel msg CONNECT");
                    let rx_buffer = tcp::SocketBuffer::new(vec![0; 16384]);
                    let tx_buffer = tcp::SocketBuffer::new(vec![0; 16384]);

                    let mut socket = tcp::Socket::new(rx_buffer, tx_buffer);

                    let local_port: u16 = 49153;
                    let remote_endpoint = IpAddress::v4(addr[0], addr[1], addr[2], addr[3]);

                    match socket.connect(self.iface.context(), (remote_endpoint, port), local_port)
                    {
                        Ok(_) => {
                            eprintln!(
                                "ENGINE New Socket local endpoint: {:?}",
                                socket.local_endpoint()
                            );
                            eprintln!(
                                "ENGINE New Socket remote endpoint: {:?}",
                                socket.remote_endpoint()
                            );

                            let handle = self.socks5_sockets.add(socket);

                            let _ = resp.send(Some(handle));
                        }
                        Err(_) => {
                            let _ = resp.send(None);
                        }
                    }
                }
                ProxyMessage::Close { handle, resp } => {
                    let _ = self.socks5_sockets.remove(handle);

                    let _ = resp.send(true);
                }
            }
        }
    }

    pub fn process_packets(&mut self) {
        let now = Instant::now();
        self.iface
            .poll(now, &mut self.device, &mut self.socks5_sockets);

        // After polling, see if any sockets have new data for SOCKS5
        for (_id, (handle, data_tx)) in self.connection_map.iter() {
            let socket = self.socks5_sockets.get_mut::<tcp::Socket>(*handle);
            if socket.can_recv() {
                let mut buf = vec![0u8; 4096];
                if let Ok(n) = socket.recv_slice(&mut buf) {
                    // Pump data back to the specific SOCKS5 task
                    let _ = data_tx.try_send(buf[..n].to_vec());
                }
            }
        }

        //POLL VPN SOCKETS
        self.iface
            .poll(now, &mut self.device, &mut self.vpn_sockets);
    }

    pub fn boring_send(&mut self, bytes: Vec<u8>) -> Result<(), TrySendError<Vec<u8>>> {
        match self.boring_send_channel.try_send(bytes) {
            Ok(()) => Ok(()),
            Err(e) => Err(e),
        }
    }

    pub fn boring_recv(&mut self) -> Result<Vec<u8>, TryRecvError> {
        match self.boring_recv_channel.try_recv() {
            Ok(msg) => Ok(msg),

            Err(e) => Err(e),
        }
    }
}
