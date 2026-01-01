use std::net::{SocketAddr, UdpSocket};
use ini::Ini;
use etherparse::PacketBuilder;
use base64::{engine::general_purpose, Engine as _};
use etherparse::{Icmpv4Header, Ipv4HeaderSlice, Ipv4Slice};


// Helper: Decode Base64 WireGuard keys to 32-byte arrays
pub fn decode_key(b64: &str) -> Result<[u8; 32], String> {
    let bytes = general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("Base64 error: {}", e))?;
    bytes
        .try_into()
        .map_err(|_| "Key must be 32 bytes".to_string())
}

// Helper: Build a raw IPv4 ICMP Echo Request
pub fn create_icmp_packet(src: [u8; 4], dst: [u8; 4]) -> Vec<u8> {
    let builder = PacketBuilder::ipv4(src, dst, 64).icmpv4_echo_request(1234, 1); // ID, Seq

    let payload = b"Hello ProtonVPN";
    let mut result = Vec::with_capacity(builder.size(payload.len()));
    builder.write(&mut result, payload).unwrap();
    result
}

// Helper: Print the result to prove it's a PONG
pub fn parse_and_print_icmp(packet: &[u8]) -> bool{
    
    // Skip IPv4 Header (usually 20 bytes) to get to ICMP
    if let Ok(ip_header) = Ipv4HeaderSlice::from_slice(packet) {
        let icmp_offset = ip_header.slice().len();
        if let Ok((icmp_header, _body)) = Icmpv4Header::from_slice(&packet[icmp_offset..]) {
            println!("   Type: {:?}", icmp_header.icmp_type); // Should be EchoReply (0)
            println!("   Source IP: {:?}", ip_header.source_addr());
            if matches!(icmp_header.icmp_type, etherparse::Icmpv4Type::EchoReply(_)) {
                println!("🎉 SUCCESS: Google replied!");
                return true
            }
        }
    }
    return false
}

pub fn get_endpoint(conf: Ini) -> SocketAddr{
    conf.get_from(Some("Peer"), "Endpoint").unwrap().parse().unwrap()
}