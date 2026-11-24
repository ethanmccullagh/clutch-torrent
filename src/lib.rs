//! The bridge as a library, used by the CLI (`main.rs`) and the Tauri app (`src-tauri/`).
//! `Bridge::start` runs the tunnel, engine, SOCKS5 server and optional port forwarding;
//! dropping the `Bridge` stops them all.

pub mod boringthreads;
pub mod event_driven_engine;
pub mod helper;
pub mod port_forward;
pub mod schema;
pub mod setup;
pub mod socks5;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result};
use ini::Ini;
use port_forward::Mapping;
use schema::ProxyMessage;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

pub const SOCKS_ADDR: &str = "127.0.0.1:1080";

pub struct Bridge {
    /// Engine and SOCKS5 server; each yields its name when it stops.
    tasks: JoinSet<&'static str>,
    tunnel_tasks: Vec<AbortHandle>,
    /// The port mapping, if forwarding was requested. Proton keeps the public port across
    /// renewals; a change is only logged, not reflected here.
    pub mapping: Option<Mapping>,
}

impl Bridge {
    /// Start everything; errors a user can fix (bad config path, SOCKS port taken, port
    /// forwarding refused) are returned. A malformed config file still panics in `setup.rs`.
    pub async fn start(config_path: &str, forward_port: Option<u16>) -> Result<Bridge> {
        let conf = Ini::load_from_file(config_path)
            .with_context(|| format!("failed to load WireGuard config {config_path}"))?;
        let listener = TcpListener::bind(SOCKS_ADDR)
            .await
            .with_context(|| format!("SOCKS5 address {SOCKS_ADDR} unavailable"))?;

        let (vpn_sender, vpn_receiver) = mpsc::unbounded_channel::<Vec<u8>>();
        let (device_sender, device_receiver) = mpsc::channel::<Vec<u8>>(100);
        let (control_sender, control_receiver) = mpsc::channel::<ProxyMessage>(100);

        let (tunn, endpoint) = setup::setup_tunnel(conf.clone());
        let tunnel_tasks =
            boringthreads::spawn_boring_threads(tunn, endpoint, vpn_receiver, device_sender)
                .await
                .context("failed to bind UDP socket for the WireGuard tunnel")?
                .iter()
                .map(|task| task.abort_handle())
                .collect();

        // From here on, an early return drops the Bridge, which stops what was started.
        let mut bridge = Bridge {
            tasks: JoinSet::new(),
            tunnel_tasks,
            mapping: None,
        };
        bridge.tasks.spawn(async move {
            event_driven_engine::run_engine(conf, device_receiver, vpn_sender, control_receiver)
                .await;
            "engine"
        });
        if let Some(port) = forward_port {
            let mapping = port_forward::start_port_forward(control_sender.clone(), port)
                .await
                .context("port forwarding failed")?;
            bridge.mapping = Some(mapping);
        }
        bridge.tasks.spawn(async move {
            if let Err(e) = socks5::serve(listener, control_sender, forward_port).await {
                eprintln!("SOCKS5 server failed: {e}");
            }
            "SOCKS5 server"
        });
        Ok(bridge)
    }

    /// Stop everything and wait until the engine and SOCKS5 server are gone, so the SOCKS5
    /// port is free again when this returns. (Just dropping the Bridge stops it in the background.)
    pub async fn stop(mut self) {
        for task in &self.tunnel_tasks {
            task.abort();
        }
        self.tasks.shutdown().await;
    }

    /// Wait until the engine or the SOCKS5 server stops; returns which one.
    pub async fn wait(&mut self) -> &'static str {
        match self.tasks.join_next().await {
            Some(Ok(name)) => name,
            Some(Err(_)) => "a task (panicked)",
            None => "nothing (no tasks)",
        }
    }
}

impl Drop for Bridge {
    // The JoinSet aborts the engine and server itself. Port forwarding and SOCKS5 connection
    // tasks end on their own once the engine's control channel closes.
    fn drop(&mut self) {
        for task in &self.tunnel_tasks {
            task.abort();
        }
    }
}
