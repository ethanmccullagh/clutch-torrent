use base64::{engine::general_purpose, Engine as _};
use etherparse::PacketBuilder;

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
