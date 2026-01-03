use crate::helper::*;
use crate::setup::*;
use boringtun::noise::{Tunn, TunnResult};
use etherparse::{Icmpv4Header, Ipv4HeaderSlice};
use smoltcp::iface::{Interface, SocketSet};
use smoltcp::socket::tcp;
use smoltcp::socket::udp::Socket;
use smoltcp::time::Instant;
use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

// --- CONFIGURATION (Replace these with your ProtonVPN details) ---
const MY_VPN_IP: [u8; 4] = [10, 2, 0, 2]; // Your interface Address (e.g. 10.2.0.2)
const TARGET_IP: [u8; 4] = [8, 8, 8, 8]; // Google DNS

pub fn google_ping_test(
    tunn: &mut Tunn,
    endpoint: &SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("--- Starting Pure User-Space WireGuard Ping ---");
    println!("Endpoint: {}", endpoint);
    println!("Target:   {:?}", TARGET_IP);

    let udp_socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    udp_socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();

    // 4. Construct the Raw ICMP Packet (Ping)
    let ping_packet = create_icmp_packet(MY_VPN_IP, TARGET_IP);

    // State tracking
    let mut handshake_completed = false;
    let mut ping_sent = false;
    let mut buf = [0u8; 2048]; // Recv buffer
    let mut out_buf = [0u8; 2048]; // Encrypt/Decrypt scratch buffer

    loop {
        // --- STEP A: SENDING LOGIC ---

        // If handshake is NOT done, trigger it by "trying" to send the packet
        if !handshake_completed {
            match tunn.encapsulate(&ping_packet, &mut out_buf) {
                TunnResult::WriteToNetwork(packet) => {
                    // BoringTun generated a Handshake Initiation packet
                    udp_socket.send_to(packet, endpoint)?;
                    println!("> Sent Handshake Initiation...");
                    // Wait a bit to avoid flooding while we wait for response
                    std::thread::sleep(Duration::from_millis(200));
                }
                _ => {} // Waiting for state machine update
            }
        }
        // If handshake IS done, send the actual Ping (once)
        else if !ping_sent {
            match tunn.encapsulate(&ping_packet, &mut out_buf) {
                TunnResult::WriteToNetwork(packet) => {
                    udp_socket.send_to(packet, endpoint)?;
                    println!("> 🚀 Encrypted PING sent to Google!");
                    ping_sent = true;
                }
                TunnResult::Err(e) => eprintln!("Encryption error: {:?}", e),
                _ => {}
            }
        }

        // --- STEP B: RECEIVING LOGIC ---

        match udp_socket.recv_from(&mut buf) {
            Ok((len, src)) => {
                if src != *endpoint {
                    continue;
                }

                // Feed the raw UDP bytes into BoringTun
                match tunn.decapsulate(None, &buf[..len], &mut out_buf) {
                    TunnResult::Done => {
                        // This usually means a Handshake Response was processed
                        if !handshake_completed {
                            println!("✅ Handshake Complete! Tunnel is UP.");
                            handshake_completed = true;
                        }
                    }
                    TunnResult::WriteToNetwork(packet) => {
                        // BoringTun wants to send a reply (e.g., keepalive or handshake response)
                        udp_socket.send_to(packet, endpoint)?;
                    }
                    TunnResult::WriteToTunnelV4(packet, _src_ip) => {
                        // Decrypted IPv4 Packet -> This is your PONG
                        println!(
                            "< 📩 Received Decrypted IPv4 Packet ({} bytes)",
                            packet.len()
                        );
                        if parse_and_print_icmp(packet) {
                            break; // Test success!
                        }
                    }
                    TunnResult::WriteToTunnelV6(packet, _src_ip) => {
                        // Decrypted IPv6 Packet
                        println!(
                            "< 📩 Received Decrypted IPv6 Packet ({} bytes)",
                            packet.len()
                        );
                    }
                    TunnResult::Err(e) => eprintln!("Tunnel Error: {:?}", e),
                    //_ => {}
                }
            }
            Err(_) => {
                // Timeout (normal, just loop again)
                tunn.update_timers(&mut out_buf); // Important: keep internal timers running
            }
        }
    }

    Ok(())
}

pub fn interface_test(
    iface: &mut Interface,
    tunnel: &mut Tunn,
    device: &mut MyVirtualDevice,
    endpoint: &SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    //interface started on 10.0.0.2/32
    let ping_packet = create_icmp_packet(MY_VPN_IP, TARGET_IP);
    let mut recv_buf = [0u8; 4096]; // Recv buffer
    let mut scratch_buf = [0u8; 4096]; // Encrypt/Decrypt scratch buffer
    let mut sockets = SocketSet::new(vec![]);
    let tcp_rx_buff = tcp::SocketBuffer::new(vec![0u8; 4096]);
    let tcp_tx_buff = tcp::SocketBuffer::new(vec![0u8; 4096]);
    let tcp_socket = tcp::Socket::new(tcp_rx_buff, tcp_tx_buff);

    let _tcp_handle = sockets.add(tcp_socket);

    let udp_socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    udp_socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();

    let mut handshake_completed = false;
    let mut handshake_sent = false;
    let mut ping_sent = false;

    loop {
        let now = Instant::now();

        if !handshake_sent {
            device.tx_queue.push_back(ping_packet.clone());
            println!("--> HANDSHAKE google ping added to transmit queue");
            handshake_sent = true;
        } else if !ping_sent {
            device.tx_queue.push_back(ping_packet.clone());
            println!("--> PING google ping added to transmit queue");
            ping_sent = true;
        }

        // 1. Let smoltcp do its work (Process TCP/IP stack)
        // This looks at rx_queue and fills tx_queue if needed
        iface.poll(now, device, &mut sockets);

        // --- OUTBOUND PATH (App -> VPN) ---
        // Take raw IP packets from smoltcp...
        while let Some(ip_packet) = device.tx_queue.pop_front() {
            // ...and feed them to BoringTun for encryption
            match tunnel.encapsulate(&ip_packet, &mut scratch_buf) {
                TunnResult::WriteToNetwork(encrypted_bytes) => {
                    // Send wireguard packet to the real internet
                    udp_socket.send_to(encrypted_bytes, &endpoint).unwrap();

                    println!(">SMOLTCP 🚀 Encrypted PING sent to Google!");
                }
                _ => {}
            }
        }

        while let Some(ip_packet) = device.rx_queue.pop_front() {
            // ...and feed them to BoringTun for encryption
            println!(">SMOLTCP packet in rx queue");
            parse_and_print_icmp(&ip_packet);
        }

        // --- INBOUND PATH (VPN -> App) ---
        // Read encrypted UDP packets from the real internet...
        if let Ok((len, src)) = udp_socket.recv_from(&mut recv_buf) {
            if src != *endpoint {
                continue;
            }

            println!("<UDP packet recieved on udp socket 0.0.0.0");
            // ...and feed them to BoringTun for decryption
            match tunnel.decapsulate(None, &recv_buf[..len], &mut scratch_buf) {
                TunnResult::Done => {
                    // This usually means a Handshake Response was processed
                    if !handshake_completed {
                        println!("✅ Handshake Complete! Tunnel is UP.");
                        handshake_completed = true;
                    }
                }
                TunnResult::WriteToTunnelV4(decrypted_bytes, _) => {
                    // Take the raw IP packet and put it in smoltcp's mailbox
                    device.rx_queue.push_back(decrypted_bytes.to_vec());

                    println!(
                        "<SMOLTCP 📩 Received Decrypted IPv4 Packet ({} bytes)",
                        decrypted_bytes.len()
                    );
                    if parse_and_print_icmp(decrypted_bytes) {
                        break; // Test success!
                    }
                }
                // Handle Handshakes/Keepalives (WriteToNetwork)
                TunnResult::WriteToNetwork(b) => {
                    udp_socket.send_to(b, endpoint).unwrap();

                    println!(
                        "<SMOLTCP Write to network/Handshake response ({} bytes)",
                        b.len()
                    );
                }
                _ => {
                    println!(">SMOLTCP other recieve");
                }
            }
        }

        //println!("looping....");
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

pub fn sock_interface_test(
    iface: &mut Interface,
    tunnel: &mut Tunn,
    device: &mut MyVirtualDevice,
    endpoint: &SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    //interface started on 10.0.0.2/32
    let ping_packet = create_icmp_packet(MY_VPN_IP, TARGET_IP);
    let mut recv_buf = [0u8; 4096]; // Recv buffer
    let mut scratch_buf = [0u8; 4096]; // Encrypt/Decrypt scratch buffer
    let mut sockets = SocketSet::new(vec![]);
    let rx_buff = tcp::SocketBuffer::new(vec![0u8; 4096]);
    let tx_buff = tcp::SocketBuffer::new(vec![0u8; 4096]);
    let tcp_socket = tcp::Socket::new(rx_buff, tx_buff);

    let _tcp_handle = sockets.add(tcp_socket);

    let udp_socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    udp_socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();

    let mut handshake_completed = false;
    let mut handshake_sent = false;
    let mut ping_sent = false;

    loop {
        let now = Instant::now();

        if !handshake_sent {
            device.tx_queue.push_back(ping_packet.clone());
            println!("--> HANDSHAKE google ping added to transmit queue");
            handshake_sent = true;
        } else if !ping_sent {
            device.tx_queue.push_back(ping_packet.clone());
            println!("--> PING google ping added to transmit queue");
            ping_sent = true;
        }

        // 1. Let smoltcp do its work (Process TCP/IP stack)
        // This looks at rx_queue and fills tx_queue if needed
        iface.poll(now, device, &mut sockets);

        // --- OUTBOUND PATH (App -> VPN) ---
        // Take raw IP packets from smoltcp...
        while let Some(ip_packet) = device.tx_queue.pop_front() {
            // ...and feed them to BoringTun for encryption
            match tunnel.encapsulate(&ip_packet, &mut scratch_buf) {
                TunnResult::WriteToNetwork(encrypted_bytes) => {
                    // Send wireguard packet to the real internet
                    udp_socket.send_to(encrypted_bytes, &endpoint).unwrap();

                    println!(">SMOLTCP 🚀 Encrypted PING sent to Google!");
                }
                _ => {}
            }
        }

        while let Some(ip_packet) = device.rx_queue.pop_front() {
            // ...and feed them to BoringTun for encryption
            println!(">SMOLTCP packet in rx queue");
            parse_and_print_icmp(&ip_packet);
        }

        // --- INBOUND PATH (VPN -> App) ---
        // Read encrypted UDP packets from the real internet...
        if let Ok((len, src)) = udp_socket.recv_from(&mut recv_buf) {
            if src != *endpoint {
                continue;
            }

            println!("<UDP packet recieved on udp socket 0.0.0.0");
            // ...and feed them to BoringTun for decryption
            match tunnel.decapsulate(None, &recv_buf[..len], &mut scratch_buf) {
                TunnResult::Done => {
                    // This usually means a Handshake Response was processed
                    if !handshake_completed {
                        println!("✅ Handshake Complete! Tunnel is UP.");
                        handshake_completed = true;
                    }
                }
                TunnResult::WriteToTunnelV4(decrypted_bytes, _) => {
                    // Take the raw IP packet and put it in smoltcp's mailbox
                    device.rx_queue.push_back(decrypted_bytes.to_vec());

                    println!(
                        "<SMOLTCP 📩 Received Decrypted IPv4 Packet ({} bytes)",
                        decrypted_bytes.len()
                    );
                    if parse_and_print_icmp(decrypted_bytes) {
                        break; // Test success!
                    }
                }
                // Handle Handshakes/Keepalives (WriteToNetwork)
                TunnResult::WriteToNetwork(b) => {
                    udp_socket.send_to(b, endpoint).unwrap();

                    println!(
                        "<SMOLTCP Write to network/Handshake response ({} bytes)",
                        b.len()
                    );
                }
                _ => {
                    println!(">SMOLTCP other recieve");
                }
            }
        }

        //println!("looping....");
        std::thread::sleep(Duration::from_millis(200));
    }
    Ok(())
}

// Helper: Print the result to prove it's a PONG
pub fn parse_and_print_icmp(packet: &[u8]) -> bool {
    // Skip IPv4 Header (usually 20 bytes) to get to ICMP
    if let Ok(ip_header) = Ipv4HeaderSlice::from_slice(packet) {
        let icmp_offset = ip_header.slice().len();
        if let Ok((icmp_header, _body)) = Icmpv4Header::from_slice(&packet[icmp_offset..]) {
            println!("   Type: {:?}", icmp_header.icmp_type); // Should be EchoReply (0)
            println!("   Source IP: {:?}", ip_header.source_addr());
            if matches!(icmp_header.icmp_type, etherparse::Icmpv4Type::EchoReply(_)) {
                println!("🎉 SUCCESS: Google replied!");
                return true;
            }
        }
    }
    return false;
}
