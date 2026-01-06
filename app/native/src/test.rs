use crate::helper::*;
use crate::schema::VpnEngine;
use boringtun::noise::TunnResult;
use etherparse::{Icmpv4Header, Ipv4HeaderSlice};
use std::time::Duration;

// --- CONFIGURATION (Replace these with your ProtonVPN details) ---
const MY_VPN_IP: [u8; 4] = [10, 2, 0, 2]; // Your interface Address (e.g. 10.2.0.2)
const TARGET_IP: [u8; 4] = [8, 8, 8, 8]; // Google DNS

pub async fn interface_test(engine: &mut VpnEngine) -> Result<(), Box<dyn std::error::Error>> {
    //interface started on 10.0.0.2/32
    let ping_packet = create_icmp_packet(MY_VPN_IP, TARGET_IP);
    let mut recv_buf = [0u8; 4096]; // Recv buffer
    let mut scratch_buf: [u8; 4096] = [0u8; 4096]; // Encrypt/Decrypt scratch buffer
    let mut handshake_completed = false;
    let mut handshake_sent = false;

    println!("STARTING TEST");

    //let tcp_set_sock = sockets.get_mut::<tcp::Socket>(tcp_handle);

    //let _ =sockets.get_mut::<tcp::Socket>(tcp_handle).connect(iface.context(), (IpAddress::v4(10,0,0,2), 80), 49152);

    //let _ = sockets.get_mut::<tcp::Socket>(tcp_handle).listen(51431);

    loop {
        if !handshake_sent {
            engine.tx_push_back(ping_packet.clone()).await;
            //let _ = sockets.get_mut::<tcp::Socket>(tcp_handle).send_slice(&mut ping_packet.clone());
            println!("--> HANDSHAKE google ping added to transmit queue");
            handshake_sent = true;
        } else {
            engine.tx_push_back(ping_packet.clone()).await;
            //let _ = sockets.get_mut::<tcp::Socket>(tcp_handle).recv_slice(&mut ping_packet.clone());
            println!("--> PING google ping added to transmit queue");
        }

        engine.process_packets().await;

        // --- OUTBOUND PATH (App -> VPN) ---
        // Take raw IP packets from smoltcp...
        while let Some(ip_packet) = engine.tx_pop_front().await {
            // ...and feed them to BoringTun for encryption
            match engine.encapsulate(&ip_packet, &mut scratch_buf).await {
                TunnResult::WriteToNetwork(encrypted_bytes) => {
                    // Send wireguard packet to the real internet
                    let _ = engine.udp_send(encrypted_bytes).await;

                    println!(">SMOLTCP 🚀 Encrypted PING sent to Google!");
                    std::thread::sleep(Duration::from_millis(200));
                }
                TunnResult::Err(e) => println!("Tunnel encap error {:?}", e),
                _ => {}
            }
        }

        // --- INBOUND PATH (VPN -> App) ---
        // Read encrypted UDP packets from the real internet...
        match engine.udp_recv(&mut recv_buf).await {
            Ok((len, src)) => {
                if src != engine.endpoint {
                    continue;
                }

                println!("<UDP packet recieved on udp socket 0.0.0.0");
                // ...and feed them to BoringTun for decryption
                match engine.decapsulate(&recv_buf[..len], &mut scratch_buf).await {
                    TunnResult::Done => {
                        // This usually means a Handshake Response was processed
                        if !handshake_completed {
                            println!("✅ Handshake Complete! Tunnel is UP.");
                            handshake_completed = true;
                        }
                    }
                    TunnResult::WriteToTunnelV4(decrypted_bytes, _) => {
                        // Take the raw IP packet and put it in smoltcp's mailbox
                        //engine.rx_push_back(decrypted_bytes.to_vec()).await;

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
                        let _ = engine.udp_send(b).await;

                        println!(
                            "<SMOLTCP Write to network/Handshake response ({} bytes)",
                            b.len()
                        );
                    }
                    TunnResult::Err(e) => {
                        eprintln!(">SMOLTCP tunnel error {:?}", e);
                    }
                    _ => {
                        println!(">SMOLTCP other recieve");
                    }
                }
            }
            Err(e) => eprintln!("{}", e),
        }
        std::thread::sleep(Duration::from_millis(200));

        println!("looping....");
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
