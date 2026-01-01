use base64::{engine::general_purpose, Engine as _};
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};
use etherparse::PacketBuilder;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};
use crate::helper::{*};

// --- CONFIGURATION (Replace these with your ProtonVPN details) ---
const PRIVATE_KEY_B64: &str = "+KTV+51/QizYMlhszn1aKW7ps79TX9FDKQb2Po9v+WI="; // e.g. "WOx+..."
const SERVER_PUBKEY_B64: &str = "KMsTNyOCInIzQ7cAFYUMyDDPhQXsTEb2T9o8gw1Hbww="; // e.g. "ABC+..."
const ENDPOINT_ADDR: &str = "89.222.98.40:51820"; // VPN Server IP:Port
const MY_VPN_IP: [u8; 4] = [10, 2, 0, 2]; // Your interface Address (e.g. 10.2.0.2)
const TARGET_IP: [u8; 4] = [8, 8, 8, 8]; // Google DNS

pub fn google_ping_test(tunn : &mut Tunn, udp_socket : UdpSocket, endpoint: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {

    println!("--- Starting Pure User-Space WireGuard Ping ---");
    println!("Endpoint: {}", endpoint);
    println!("Target:   {:?}", TARGET_IP);


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
                if src != endpoint {
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
                    _ => {}
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


