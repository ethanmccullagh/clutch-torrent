//! Tests that run against a real WireGuard tunnel. They need the config (`$WG_CONFIG_PATH`,
//! default `cachy3-CA-1208.conf` in the repo root) and network access.
//!
//! - `tunnel`:  raw packets through boringtun, no engine
//! - `icmp`, `tcp`, `udp`, `nat_pmp`: engine sockets driven by `ProxyMessage`s
//! - `socks5`:  the SOCKS5 server on 127.0.0.1:1080

mod icmp;
mod nat_pmp;
mod socks5;
mod tcp;
mod tunnel;
mod udp;

use crate::boringthreads::spawn_boring_threads;
use crate::event_driven_engine::run_engine;
use crate::schema::{ConnectMessage, ProxyMessage, SocketReadChannel};
use crate::setup::setup_tunnel;
use crate::socks5::run_server;
use ini::Ini;
use smoltcp::iface::SocketHandle;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Mutex, MutexGuard};

// Every test opens its own WireGuard session with the same key; the server keeps one session per peer, so parallel tests break each other.
static TUNNEL_LOCK: Mutex<()> = Mutex::const_new(());

/// Hold the returned guard for the whole test.
async fn lock_tunnel() -> MutexGuard<'static, ()> {
    TUNNEL_LOCK.lock().await
}

fn load_config() -> Ini {
    let path =
        std::env::var("WG_CONFIG_PATH").unwrap_or_else(|_| "cachy3-CA-1208.conf".to_string());
    Ini::load_from_file(path).expect("Failed to load WG config for testing")
}

/// Start the tunnel, the engine and the SOCKS5 server; return the engine's control channel.
async fn start_test_environment() -> mpsc::Sender<ProxyMessage> {
    let conf = load_config();

    let (vpn_sender, vpn_receiver) = mpsc::channel::<Vec<u8>>(100);
    let (device_sender, device_receiver) = mpsc::channel::<Vec<u8>>(100);
    let (control_sender, control_receiver) = mpsc::channel::<ProxyMessage>(100);

    let (tunn, endpoint) = setup_tunnel(conf.clone());
    spawn_boring_threads(tunn, endpoint, vpn_receiver, device_sender)
        .await
        .expect("Failed to spawn boring threads");

    tokio::spawn(run_engine(
        conf,
        device_receiver,
        vpn_sender,
        control_receiver,
    ));

    let server_control = control_sender.clone();
    tokio::spawn(async move {
        let _ = run_server(server_control).await;
    });

    // Give the WireGuard handshake a moment
    tokio::time::sleep(Duration::from_millis(1000)).await;

    control_sender
}

/// Send a Connect message; return the socket handle and its read channel.
async fn connect(
    control: &mpsc::Sender<ProxyMessage>,
    make: impl FnOnce(
        oneshot::Sender<Option<SocketHandle>>,
        mpsc::Sender<SocketReadChannel>,
    ) -> ConnectMessage,
) -> (SocketHandle, mpsc::Receiver<SocketReadChannel>) {
    let (resp, handle) = oneshot::channel();
    let (read_tx, read) = mpsc::channel::<SocketReadChannel>(10);
    control
        .send(ProxyMessage::Connect {
            msg: make(resp, read_tx),
        })
        .await
        .unwrap();
    (handle.await.unwrap().expect("socket creation failed"), read)
}

/// Wait up to 5 s for one received packet; panic on timeout, EOF or closed channel.
async fn recv_packet(
    read: &mut mpsc::Receiver<SocketReadChannel>,
) -> (bytes::Bytes, smoltcp::wire::IpAddress) {
    match tokio::time::timeout(Duration::from_secs(5), read.recv()).await {
        Ok(Some(Some(packet))) => packet,
        Ok(Some(None)) => panic!("got EOF instead of data"),
        Ok(None) => panic!("read channel closed"),
        Err(_) => panic!("timed out waiting for data"),
    }
}
