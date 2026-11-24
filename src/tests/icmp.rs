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

    // Retry: the server's MTU probing at session start can drop the first ping (seen in the wild).
    let mut got_reply = false;
    'ping: for seq in 1..=5 {
        control
            .send(ProxyMessage::Write {
                msg: WriteMessage::ICMP {
                    handle,
                    dst: [8, 8, 8, 8],
                    ident: 0x1234,
                    seq,
                },
            })
            .await
            .unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            match tokio::time::timeout_at(deadline, read.recv()).await {
                Ok(Some(Some((data, from)))) => {
                    if parse_and_print_icmp_noip(&data, from) {
                        got_reply = true;
                        break 'ping;
                    }
                    // The server's MTU probes are echo requests with our ident; they reach this socket too.
                    println!(
                        "skipping non-reply ICMP packet ({} bytes) from {from}",
                        data.len()
                    );
                }
                Ok(Some(None)) | Ok(None) => panic!("ICMP socket closed"),
                Err(_) => {
                    println!("no reply to ping seq={seq}, retrying");
                    break;
                }
            }
        }
    }
    assert!(got_reply, "no ICMP echo reply after 5 pings");
}
