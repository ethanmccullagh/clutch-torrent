use crate::schema::*;
use smoltcp::time::Instant;
use std::time::Duration;
use tokio::sync::mpsc;

async fn run_engine(mut engine: VpnEngine, mut proxy_rx: mpsc::Receiver<ProxyMessage>) {
    let mut recv_buf = [0u8; 4096]; // Recv buffer
    let mut vpn_buf = [0u8; 4096]; // Encrypt/Decrypt scratch buffer
    loop {
        let now = Instant::now();

        // 1. Let smoltcp process its internal state
        // This returns a "poll_delay" — how long until the NEXT thing needs to happen
        engine.process_packets().await;

        let poll_delay = engine
            .poll_delay(now)
            .map(|d| Duration::from_micros(d.total_micros()))
            .unwrap_or(Duration::from_millis(100)); // Default sleep if idle

        // 2. Wait for the NEXT event
        tokio::select! {
            // Wait for data from Libtransmission
            msg = proxy_rx.recv() => {
                if let Some(m) = msg {
                    //handle_message(m, &mut engine);
                }
            }

            // Wait for incoming UDP from ProtonVPN
            Ok((len, addr)) = engine.udp_recv(&mut recv_buf) => {
                let _ = engine.decapsulate(&recv_buf[..len], &mut vpn_buf);
                // Push decrypted IP packet into smoltcp device
                engine.rx_push_back(vpn_buf.to_vec()).await;
            }

            // Wait for smoltcp's next scheduled task (retransmission/timer)
            _ = tokio::time::sleep(poll_delay) => {
                // Just wake up and loop to trigger iface.poll()
            }
        }
    }
}
