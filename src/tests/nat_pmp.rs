use super::*;
use crate::schema::WriteMessage;
use bytes::Bytes;
use smoltcp::wire::IpAddress;

/// Send a NAT-PMP request from a UDP socket on `local_port` to the VPN gateway.
/// Returns the reply after checking the header every reply shares.
async fn request(local_port: u16, opcode: u8) -> Bytes {
    let _tunnel = lock_tunnel().await;
    let control = start_test_environment().await;
    let (handle, mut read) = connect(&control, |resp, read| ConnectMessage::UDP {
        local_port,
        resp,
        read,
    })
    .await;

    control
        .send(ProxyMessage::Write {
            msg: WriteMessage::NAT { handle, opcode },
        })
        .await
        .unwrap();

    let (reply, from) = recv_packet(&mut read).await;
    control.send(ProxyMessage::Close).await.unwrap();

    assert_eq!(from, IpAddress::v4(10, 2, 0, 1), "reply not from gateway");
    assert_eq!(reply[0], 0, "NAT-PMP version");
    assert_eq!(reply[1], 128 + opcode, "reply opcode");
    assert_eq!(
        u16::from_be_bytes([reply[2], reply[3]]),
        0,
        "NAT-PMP result code"
    );
    reply
}

#[tokio::test]
async fn external_address() {
    let reply = request(5000, 0).await;
    assert_eq!(reply.len(), 12);
    let ip = std::net::Ipv4Addr::new(reply[8], reply[9], reply[10], reply[11]);
    assert!(
        !ip.is_unspecified() && !ip.is_private(),
        "external address {ip}"
    );
}

/// Creates a real mapping on the VPN server that lasts an hour (the engine hardcodes the lifetime).
#[tokio::test]
async fn udp_mapping() {
    let reply = request(5000, 1).await;
    assert_eq!(reply.len(), 16);
    assert_eq!(
        u16::from_be_bytes([reply[8], reply[9]]),
        5000,
        "internal port"
    );
    assert_ne!(
        u16::from_be_bytes([reply[10], reply[11]]),
        0,
        "external port"
    );
    let lifetime = u32::from_be_bytes([reply[12], reply[13], reply[14], reply[15]]);
    assert!(lifetime > 0, "mapping lifetime");
}
