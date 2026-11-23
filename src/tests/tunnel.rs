use super::*;
use crate::helper::{create_icmp_packet, parse_and_print_icmp};

/// A hand-built ping goes through boringtun and the reply comes back, without the engine.
#[tokio::test]
async fn raw_ping() {
    let _tunnel = lock_tunnel().await;

    let (vpn_sender, vpn_receiver) = mpsc::channel::<Vec<u8>>(100);
    let (device_sender, mut device_receiver) = mpsc::channel::<Vec<u8>>(100);

    let (tunn, endpoint) = setup_tunnel(load_config());
    let _handles = spawn_boring_threads(tunn, endpoint, vpn_receiver, device_sender)
        .await
        .expect("Failed to spawn boring threads");

    vpn_sender
        .send(create_icmp_packet([10, 2, 0, 2], [8, 8, 8, 8]))
        .await
        .unwrap();

    let got_reply = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(packet) = device_receiver.recv().await {
            if parse_and_print_icmp(&packet) {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false);

    assert!(got_reply, "Ping response not received or invalid");
}
