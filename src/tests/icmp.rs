use super::*;
use crate::helper::parse_and_print_icmp_noip;
use crate::schema::WriteMessage;

/// Ping 8.8.8.8 through an engine ICMP socket.
#[tokio::test]
async fn echo() {
    let _tunnel = lock_tunnel().await;
    let control = start_test_environment().await;
    let (handle, mut read) =
        connect(&control, |resp, read| ConnectMessage::ICMP { resp, read }).await;

    control
        .send(ProxyMessage::Write {
            msg: WriteMessage::ICMP {
                handle,
                dst: [8, 8, 8, 8],
                ident: 0x1234,
                seq: 1,
            },
        })
        .await
        .unwrap();

    let (data, from) = recv_packet(&mut read).await;
    assert!(
        parse_and_print_icmp_noip(&data, from),
        "ICMP response failed"
    );

    control.send(ProxyMessage::Close).await.unwrap();
}
