use base64::{engine::general_purpose, Engine as _};
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};
use etherparse::PacketBuilder;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

// --- CONFIGURATION (Replace these with your ProtonVPN details) ---
const PRIVATE_KEY_B64: &str = "+KTV+51/QizYMlhszn1aKW7ps79TX9FDKQb2Po9v+WI="; // e.g. "WOx+..."
const SERVER_PUBKEY_B64: &str = "KMsTNyOCInIzQ7cAFYUMyDDPhQXsTEb2T9o8gw1Hbww="; // e.g. "ABC+..."
const ENDPOINT_ADDR: &str = "89.222.98.40:51820"; // VPN Server IP:Port
const MY_VPN_IP: [u8; 4] = [10, 2, 0, 2]; // Your interface Address (e.g. 10.2.0.2)
const TARGET_IP: [u8; 4] = [8, 8, 8, 8]; // Google DNS

pub fn test() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Decode Keys
    let private_key = decode_key(PRIVATE_KEY_B64)?;
    let peer_public_key = decode_key(SERVER_PUBKEY_B64)?;
    let endpoint: SocketAddr = ENDPOINT_ADDR.parse()?;

    println!("--- Starting Pure User-Space WireGuard Ping ---");
    println!("Endpoint: {}", endpoint);
    println!("Target:   {:?}", TARGET_IP);

    // 2. Setup UDP Socket (The only connection to the OS)
    let udp_socket = UdpSocket::bind("0.0.0.0:0")?;
    udp_socket.set_read_timeout(Some(Duration::from_millis(100)))?;

    // 3. Initialize BoringTun State Machine
    // Using static_client_curve25519 is the standard for WireGuard clients
    let mut tunn = Tunn::new(
        StaticSecret::from(private_key),
        PublicKey::from(peer_public_key),
        None,     // Preshared Key (Proton usually doesn't use this, check your config)
        Some(25), // Persistent Keepalive
        0,        // Index
        None,     // Rate Limiter
    )
    .map_err(|e| format!("Tunnel init failed: {}", e))?;

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
                        parse_and_print_icmp(packet);
                        break; // Test success!
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

// Helper: Decode Base64 WireGuard keys to 32-byte arrays
fn decode_key(b64: &str) -> Result<[u8; 32], String> {
    let bytes = general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("Base64 error: {}", e))?;
    bytes
        .try_into()
        .map_err(|_| "Key must be 32 bytes".to_string())
}

// Helper: Build a raw IPv4 ICMP Echo Request
fn create_icmp_packet(src: [u8; 4], dst: [u8; 4]) -> Vec<u8> {
    let builder = PacketBuilder::ipv4(src, dst, 64).icmpv4_echo_request(1234, 1); // ID, Seq

    let payload = b"Hello ProtonVPN";
    let mut result = Vec::with_capacity(builder.size(payload.len()));
    builder.write(&mut result, payload).unwrap();
    result
}

// Helper: Print the result to prove it's a PONG
fn parse_and_print_icmp(packet: &[u8]) {
    use etherparse::Icmpv4Header;
    use etherparse::Ipv4HeaderSlice;

    // Skip IPv4 Header (usually 20 bytes) to get to ICMP
    if let Ok(ip_header) = Ipv4HeaderSlice::from_slice(packet) {
        let icmp_offset = ip_header.slice().len();
        if let Ok((icmp_header, _body)) = Icmpv4Header::from_slice(&packet[icmp_offset..]) {
            println!("   Type: {:?}", icmp_header.icmp_type); // Should be EchoReply (0)
            println!("   Source IP: {:?}", ip_header.source_addr());
            if matches!(icmp_header.icmp_type, etherparse::Icmpv4Type::EchoReply(_)) {
                println!("🎉 SUCCESS: Google replied!");
            }
        }
    }
}
