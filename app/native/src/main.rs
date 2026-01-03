use boringtun::noise::{Tunn, TunnResult};
use ini::Ini;
use managed::ManagedSlice;
use smoltcp::iface::{Interface, SocketSet};
use smoltcp::socket::tcp;
use smoltcp::socket::udp::Socket;
use smoltcp::time::Instant;
use std::net::UdpSocket;
use std::net::SocketAddr;
use std::time::Duration;
use std::net::IpAddr;

pub mod helper;
pub mod setup;
pub mod test;

//mod test;
use setup::*;
use test::*;

/// A simple Virtual Device that stores packets in a Vec

//#[tokio::main]
fn main() {
    //load config file from path
    let conf = Ini::load_from_file("/home/ethanm/Downloads/cachy-CA-1149.conf").unwrap();

    let mut tunnel = setup_tunnel(conf.clone()).unwrap();

    let (mut iface, mut device) = setup_smoltcp_iface(conf.clone()).unwrap();

    let endpoint: SocketAddr = conf.get_from(Some("Peer"), "Endpoint").unwrap().parse().unwrap();

    

    //TEST: ping google
    //let _ = google_ping_test(&mut tunnel, &udp_socket, &endpoint);

    


    //TEST: ping google using smoltcp interface
    let _ = interface_test(&mut iface, &mut tunnel, &mut device, &endpoint);

    //event_loop(&mut iface, &mut tunnel, &mut device, &mut sockets, &mut scratch_buf, &endpoint, &udp_socket, &mut recv_buf);
}

fn event_loop(
    iface: &mut Interface,
    tunnel: &mut Tunn,
    device: &mut MyVirtualDevice,
    sockets: &mut SocketSet<'_>,
    scratch_buf: &mut [u8; 2048],
    endpoint: &SocketAddr,
    udp_socket: &UdpSocket,
    recv_buf: &mut [u8; 2048]
) {
    loop {
        let now = Instant::now();

        // 1. Let smoltcp do its work (Process TCP/IP stack)
        // This looks at rx_queue and fills tx_queue if needed
        iface.poll(now, device, sockets);

        // --- OUTBOUND PATH (App -> VPN) ---
        // Take raw IP packets from smoltcp...
        while let Some(ip_packet) = device.tx_queue.pop_front() {
            // ...and feed them to BoringTun for encryption
            match tunnel.encapsulate(&ip_packet, scratch_buf) {
                TunnResult::WriteToNetwork(encrypted_bytes) => {
                    // Send wireguard packet to the real internet
                    udp_socket.send_to(encrypted_bytes, &endpoint).unwrap();
                }
                _ => {}
            }
        }

        // --- INBOUND PATH (VPN -> App) ---
        // Read encrypted UDP packets from the real internet...
        if let Ok((len, _)) = udp_socket.recv_from(recv_buf) {
            // ...and feed them to BoringTun for decryption
            match tunnel.decapsulate(None, &recv_buf[..len], scratch_buf) {
                TunnResult::WriteToTunnelV4(decrypted_bytes, _) => {
                    // Take the raw IP packet and put it in smoltcp's mailbox
                    device.rx_queue.push_back(decrypted_bytes.to_vec());
                }
                // Handle Handshakes/Keepalives (WriteToNetwork)
                TunnResult::WriteToNetwork(b) => {
                    udp_socket.send_to(b, endpoint).unwrap();
                }
                _ => {}
            }
        }
    }
}
