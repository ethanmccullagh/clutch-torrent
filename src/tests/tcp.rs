use super::*;
use crate::schema::WriteMessage;
use bytes::Bytes;
use smoltcp::wire::{IpAddress, IpEndpoint};

/// Echo a line through the TCP echo server at 45.79.112.203:4242.
#[tokio::test]
async fn echo() {
    let _tunnel = lock_tunnel().await;
    let control = start_test_environment().await;
    let (handle, mut read) = connect(&control, |resp, read| ConnectMessage::TCP {
        remote: IpEndpoint::new(IpAddress::v4(45, 79, 112, 203), 4242),
        local_port: 0,
        connect: true,
        resp,
        read,
    })
    .await;

    // Written before the handshake finishes: the engine queues it until the socket can send.
    control
        .send(ProxyMessage::Write {
            msg: WriteMessage::TCP {
                handle,
                data: Bytes::from_static(b"Hello TCP\n"),
            },
        })
        .await
        .unwrap();

    let (data, _) = recv_packet(&mut read).await;
    assert_eq!(&data[..], b"Hello TCP\n");

    control.send(ProxyMessage::Close).await.unwrap();
}
