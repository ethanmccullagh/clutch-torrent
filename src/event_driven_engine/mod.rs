//! The engine: a tokio task that owns the smoltcp stack and bridges it to the tunnel and to clients.
//!
//! It sleeps until one of these happens, then moves data until nothing more can move:
//!   1. a packet arrives from the tunnel (`device_receiver`)
//!   2. a control message arrives (`control_receiver`)
//!   3. a reader made room in, or dropped, its read channel (`wake`)
//!   4. smoltcp's next timer fires (`poll_delay`: retransmits, keepalives, ...)

mod device;
mod sockets;

use crate::schema::ProxyMessage;
use ini::Ini;
use sockets::Engine;
use tokio::sync::mpsc;

pub async fn run_engine(
    conf: Ini,
    mut device_receiver: mpsc::Receiver<Vec<u8>>,
    vpn_sender: mpsc::Sender<Vec<u8>>,
    mut control_receiver: mpsc::Receiver<ProxyMessage>,
) {
    let (wake_tx, mut wake_rx) = mpsc::unbounded_channel();
    let mut engine = Engine::new(conf, vpn_sender, wake_tx);

    loop {
        engine.service();
        let timer = engine.poll_delay();

        tokio::select! {
            Some(packet) = device_receiver.recv() => {
                engine.push_packet(packet);
                // Take the rest of a burst so one service pass handles it.
                while let Ok(packet) = device_receiver.try_recv() {
                    engine.push_packet(packet);
                }
            }
            msg = control_receiver.recv() => match msg {
                Some(ProxyMessage::Connect { msg }) => engine.connect(msg),
                Some(ProxyMessage::Write { msg }) => engine.write(msg),
                Some(ProxyMessage::Close) | None => break,
            },
            Some(handle) = wake_rx.recv() => engine.unblock(handle),
            _ = sleep_for(timer) => {}
        }
    }
}

/// Sleep for smoltcp's requested delay, or forever if it has no pending timers.
async fn sleep_for(delay: Option<smoltcp::time::Duration>) {
    match delay {
        Some(d) => tokio::time::sleep(d.into()).await,
        None => std::future::pending().await,
    }
}
