use super::*;
use crate::schema::WriteMessage;
use bytes::Bytes;
use smoltcp::wire::IpAddress;

/// Resolve example.com with a DNS query to 8.8.8.8:53.
#[tokio::test]
async fn dns_query() {
    let _tunnel = lock_tunnel().await;
    let control = start_test_environment().await;
    let (handle, mut read) = connect(&control, |resp, read| ConnectMessage::UDP {
        local_port: 0,
        resp,
        read,
    })
    .await;

    // DNS query: id 0xBEEF, recursion desired, one question: example.com A IN
    let mut query = vec![0xBE, 0xEF, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    query.extend_from_slice(b"\x07example\x03com\x00");
    query.extend_from_slice(&[0, 1, 0, 1]);

    control
        .send(ProxyMessage::Write {
            msg: WriteMessage::UDP {
                handle,
                data: Bytes::from(query),
                dst: [8, 8, 8, 8],
                dst_port: 53,
            },
        })
        .await
        .unwrap();

    let (reply, from) = recv_packet(&mut read).await;
    assert_eq!(from, IpAddress::v4(8, 8, 8, 8));
    assert_eq!(&reply[..2], &[0xBE, 0xEF], "reply id mismatch");
    assert!(reply[2] & 0x80 != 0, "not a DNS response");
    assert_eq!(reply[3] & 0x0F, 0, "DNS error rcode");
    assert!(u16::from_be_bytes([reply[6], reply[7]]) > 0, "no answers");
}

/// A UDP reply larger than 256 bytes (the old socket buffer size) must reach the reader.
#[tokio::test]
async fn large_reply() {
    let _tunnel = lock_tunnel().await;
    let control = start_test_environment().await;
    let (handle, mut read) = connect(&control, |resp, read| ConnectMessage::UDP {
        local_port: 0,
        resp,
        read,
    })
    .await;

    // DNS query: google.com TXT IN, plus an EDNS OPT record allowing 1232-byte replies
    // (without it the answer exceeds 512 bytes and comes back empty and truncated).
    let mut query = vec![0xBE, 0xEF, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 1];
    query.extend_from_slice(b"\x06google\x03com\x00");
    query.extend_from_slice(&[0, 16, 0, 1]);
    query.extend_from_slice(&[0, 0, 41, 0x04, 0xD0, 0, 0, 0, 0, 0, 0]);

    control
        .send(ProxyMessage::Write {
            msg: WriteMessage::UDP {
                handle,
                data: Bytes::from(query),
                dst: [8, 8, 8, 8],
                dst_port: 53,
            },
        })
        .await
        .unwrap();

    let (reply, _) = recv_packet(&mut read).await;
    assert!(reply.len() > 256, "reply only {} bytes", reply.len());
}
