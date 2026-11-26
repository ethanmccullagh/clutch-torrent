use super::*;
use crate::helper::{create_icmp_packet, parse_and_print_icmp};

/// A hand-built ping goes through boringtun and the reply comes back, without the engine.
#[tokio::test]
async fn raw_ping() {
    let _tunnel = lock_tunnel().await;
    let env = test_env().await;
    let mut tap = env.tap.subscribe();

    env.vpn
        .send(create_icmp_packet([10, 2, 0, 2], [8, 8, 8, 8]))
        .unwrap();

    let got_reply = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match tap.recv().await {
                Ok(packet) if parse_and_print_icmp(&packet) => return true,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return false,
            }
        }
    })
    .await
    .unwrap_or(false);

    assert!(got_reply, "Ping response not received or invalid");
}
