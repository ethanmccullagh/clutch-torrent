//! The bridge as a library, used by the CLI (`main.rs`) and the Tauri app (`src-tauri/`).
//! `Bridge::start` runs the tunnel, engine, SOCKS5 server and optional port forwarding;
//! dropping the `Bridge` stops them all.
//!
//! The caller binds the SOCKS5 listener (`bind_socks`) and keeps it. While the bridge is stopped,
//! `refuse_connections` holds the port, so no other program can take it and receive traffic
//! meant for the VPN (the kill switch relies on that).

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
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinSet};

pub const DEFAULT_SOCKS_PORT: u16 = 1080;

/// Bind the SOCKS5 listener on localhost. Port 0 picks a free port.
pub async fn bind_socks(port: u16) -> Result<Arc<TcpListener>> {
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("proxy port {port} on 127.0.0.1 is unavailable"))?;
    Ok(Arc::new(listener))
}

/// Hold the SOCKS5 port while the bridge is stopped: accept and close every connection, so
/// clients fail at once instead of hanging, and no other program can bind the port. Runs until
/// aborted; abort it before starting a bridge on the same listener.
pub async fn refuse_connections(listener: Arc<TcpListener>) {
    loop {
        if let Ok((stream, _)) = listener.accept().await {
            drop(stream);
        }
    }
}

pub struct Bridge {
    /// Engine and SOCKS5 server; each yields its name when it stops.
    tasks: JoinSet<&'static str>,
    tunnel_tasks: Vec<AbortHandle>,
    /// The port mapping, if forwarding was requested. Proton keeps the public port across
    /// renewals; a change is only logged, not reflected here.
    pub mapping: Option<Mapping>,
}

impl Bridge {
    /// Start everything, serving SOCKS5 on `socks` (from `bind_socks`). Errors a user can fix
    /// (bad config path, port forwarding refused) are returned. A malformed config file still
    /// panics in `setup.rs`.
    pub async fn start(
        config_path: &str,
        forward_port: Option<u16>,
        socks: Arc<TcpListener>,
    ) -> Result<Bridge> {
        let conf = Ini::load_from_file(config_path)
            .with_context(|| format!("failed to load WireGuard config {config_path}"))?;

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
            if let Err(e) = socks5::serve(socks, control_sender, forward_port).await {
                eprintln!("SOCKS5 server failed: {e}");
            }
            "SOCKS5 server"
        });
        Ok(bridge)
    }

    /// Stop everything and wait until the engine and SOCKS5 server are gone, so nothing else is
    /// accepting on the SOCKS5 listener when this returns. (Dropping the Bridge stops it in the
    /// background.)
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

#[cfg(test)]
mod socks_port_tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// While refusing, the port stays taken and connections are closed at once.
    #[tokio::test]
    async fn refused_port_is_held_and_closes_connections() {
        let listener = bind_socks(0).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let refuser = tokio::spawn(refuse_connections(listener.clone()));

        assert!(
            bind_socks(port).await.is_err(),
            "another listener could take the port"
        );

        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        let mut buf = [0u8; 1];
        let read = tokio::time::timeout(std::time::Duration::from_secs(2), client.read(&mut buf))
            .await
            .expect("connection hung instead of being closed");
        assert!(
            matches!(read, Ok(0) | Err(_)),
            "connection was served: {read:?}"
        );
        refuser.abort();
    }
}
