//! The torrent engine: librqbit, with its traffic sent through the bridge's SOCKS5 proxy.
//!
//! librqbit can proxy only TCP: peer connections and HTTP(S) tracker requests. Everything that
//! would use UDP or multicast from the real IP is off: DHT, local service discovery, and the
//! listener (which also brings uTP and UPnP).
//!
//! UDP trackers: librqbit announces to `udp://` trackers directly, and there's no option to turn
//! off only those, so `add` removes them first (`udp_trackers.rs`).

use librqbit::{
    AddTorrent, AddTorrentResponse, ConnectionOptions, ManagedTorrent, Session, SessionOptions,
};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

/// Start the torrent session. `proxy_port`: the bridge's SOCKS5 port on 127.0.0.1, which the app
/// must hold for as long as the session lives (see `native::refuse_connections`).
pub async fn start_session(download_dir: PathBuf, proxy_port: u16) -> anyhow::Result<Arc<Session>> {
    Session::new_with_opts(download_dir, session_options(proxy_port)).await
}

/// Add a magnet link or .torrent file. Every torrent must be added through here, so UDP
/// trackers are removed first.
#[allow(dead_code)] // no add_torrent command yet; the tests use it
pub async fn add(
    session: &Arc<Session>,
    add: AddTorrent<'_>,
) -> anyhow::Result<Arc<ManagedTorrent>> {
    let add = crate::udp_trackers::strip(add)?;
    match session.add_torrent(add, None).await? {
        AddTorrentResponse::Added(_, torrent) | AddTorrentResponse::AlreadyManaged(_, torrent) => {
            Ok(torrent)
        }
        AddTorrentResponse::ListOnly(_) => anyhow::bail!("torrent was only listed, not added"),
    }
}

fn session_options(proxy_port: u16) -> SessionOptions {
    SessionOptions {
        // ---- Routing through the VPN ----
        // Every outgoing peer connection and HTTP(S) tracker request goes through the proxy.
        // socks5h sends tracker hostnames to the bridge, so DNS is resolved inside the tunnel.
        connect: Some(ConnectionOptions {
            proxy_url: Some(format!("socks5h://127.0.0.1:{proxy_port}")),
            ..Default::default()
        }),
        // The tunnel is IPv4 only, so IPv6 peers can't be reached through it.
        ipv4_only: true,
        // Binds sockets to a real network interface. The VPN is not an interface here (the
        // tunnel runs in user space), so this stays unset.
        bind_device_name: None,

        // ---- Off: these open their own sockets, outside the proxy ----
        // DHT: UDP from the real IP.
        dht: None,
        // Local service discovery: multicast on the local network.
        disable_local_service_discovery: true,
        // No listener, so no incoming connections, uTP or UPnP. With port forwarding, this
        // would listen on 127.0.0.1:<forward port> and announce the public port
        // (`ListenerOptions::announce_port`).
        listen: None,

        // ---- Trackers ----
        // HTTP(S) trackers go through the proxy; UDP trackers don't, so `add` removes them.
        disable_trackers: false,
        // Extra trackers added to every torrent; they must be HTTP(S) too.
        trackers: HashSet::new(),

        // ---- Remembering torrents across restarts ----
        // Placeholder: `Some(SessionPersistenceConfig::Json { folder: Some(<app data dir>) })`.
        persistence: None,
        // Restore progress without rechecking every piece (needs persistence).
        fastresume: false,

        // ---- Limits and identity ----
        peer_limit: None,
        ratelimits: Default::default(),
        peer_id: None,
        client_name_and_version: None,
        // Fetched over HTTP at startup, through the proxy.
        blocklist_url: None,
        allowlist_url: None,

        // Not relevant here: storage, runtime and tracing settings.
        ..Default::default()
    }
}

/// These tests use the real VPN: they need the WireGuard config, network access, and nothing
/// else using the key at the same time (not the app, not the bridge tests). They fetch Ubuntu's
/// .torrent file directly (outside the VPN); only the torrent traffic itself is under test.
///
/// One bridge (one tunnel, one handshake), session and torrent are shared by every test, started
/// once by `env()`, and each test holds `LOCK` so they run one at a time even with
/// `--test-threads`.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::LazyLock;
    use std::time::Duration;
    use tokio::runtime::Runtime;
    use tokio::sync::{Mutex, MutexGuard, OnceCell};

    /// The shared bridge and session run here, not on a test's runtime, which is shut down (with
    /// every task spawned on it) when that test ends.
    static RUNTIME: LazyLock<Runtime> =
        LazyLock::new(|| Runtime::new().expect("failed to build the shared test runtime"));
    static ENV: OnceCell<Env> = OnceCell::const_new();
    static LOCK: Mutex<()> = Mutex::const_new(());

    struct Env {
        _bridge: native::Bridge,
        _session: Arc<Session>,
        torrent: Arc<ManagedTorrent>,
        /// The WireGuard server, `ip:port`.
        endpoint: String,
    }

    /// The shared environment and the lock that keeps tests one at a time.
    async fn env() -> (MutexGuard<'static, ()>, &'static Env) {
        let guard = LOCK.lock().await;
        let env = ENV
            .get_or_init(|| async { RUNTIME.spawn(start()).await.expect("test setup failed") })
            .await;
        (guard, env)
    }

    /// Start a bridge on a free port, a session through it, and add Ubuntu's server ISO
    /// torrent (HTTPS tracker only).
    async fn start() -> Env {
        let config = std::env::var("WG_CONFIG_PATH")
            .unwrap_or_else(|_| "../cachy3-CA-1208.conf".to_string());
        let socks = native::bind_socks(0).await.unwrap();
        let proxy_port = socks.local_addr().unwrap().port();
        let bridge = native::Bridge::start(&config, None, socks).await.unwrap();
        let endpoint = ini::Ini::load_from_file(&config).unwrap()["Peer"]["Endpoint"].to_string();

        let base = "https://releases.ubuntu.com/24.04/";
        let curl = |url: &str| {
            std::process::Command::new("curl")
                .args(["-sf", url])
                .output()
                .unwrap()
                .stdout
        };
        let listing = String::from_utf8(curl(base)).unwrap();
        let name = listing
            .split('"')
            .filter(|s| s.starts_with("ubuntu-") && s.ends_with("-live-server-amd64.iso.torrent"))
            .max() // newest point release
            .expect("no server ISO torrent in Ubuntu's listing");
        let file = curl(&format!("{base}{name}"));

        // Fixed path, cleared here: the shared session is never stopped, so the files outlive
        // the run until the next one.
        let dir = std::env::temp_dir().join("clutch-test");
        let _ = std::fs::remove_dir_all(&dir);
        let session = start_session(dir, proxy_port).await.unwrap();
        let torrent = add(&session, AddTorrent::from_bytes(file)).await.unwrap();
        Env {
            _bridge: bridge,
            _session: session,
            torrent,
            endpoint,
        }
    }

    impl Env {
        /// Poll the torrent's peer stats once a second until `done` or `secs` pass.
        async fn wait_for(
            &self,
            secs: u64,
            done: impl Fn(&librqbit::TorrentStats) -> bool,
        ) -> bool {
            for _ in 0..secs {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if done(&self.torrent.stats()) {
                    return true;
                }
            }
            false
        }
    }

    /// The HTTPS tracker answers through the VPN: with DHT off, every peer the torrent knows
    /// about came from a tracker announce sent through the proxy.
    #[tokio::test(flavor = "multi_thread")]
    async fn tracker_reachable_through_vpn() {
        let (_guard, env) = env().await;
        let reached = env
            .wait_for(60, |s| {
                s.live
                    .as_ref()
                    .is_some_and(|l| l.snapshot.peer_stats.seen > 0)
            })
            .await;
        assert!(reached, "no peers from the tracker within 60 s");
    }

    /// Download until a peer sends data, then check that the process has no connected sockets
    /// to anything but localhost and the WireGuard server, and no direct peer connections.
    /// Limit: `ss` only lists connected sockets, so UDP sent from an unconnected socket (a UDP
    /// tracker announce, say) wouldn't show up here.
    #[tokio::test(flavor = "multi_thread")]
    async fn downloads_through_vpn_without_leaks() {
        let (_guard, env) = env().await;
        let downloaded = env
            .wait_for(90, |s| {
                s.progress_bytes > 0
                    && s.live
                        .as_ref()
                        .is_some_and(|l| l.snapshot.peer_stats.live_socks > 0)
            })
            .await;
        // (through the proxy, direct TCP, direct uTP)
        let peers = env.torrent.stats().live.map(|l| {
            let p = l.snapshot.peer_stats;
            (p.live_socks, p.live_tcp, p.live_utp)
        });

        let pid = format!("pid={},", std::process::id());
        let sockets = String::from_utf8(
            std::process::Command::new("ss")
                .args(["-tunpH"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let ours: Vec<&str> = sockets.lines().filter(|l| l.contains(&pid)).collect();
        let leaks: Vec<&str> = ours
            .iter()
            .copied()
            .filter(|line| {
                let peer = line.split_whitespace().nth(5).unwrap_or("");
                !(peer.starts_with("127.0.0.1:")
                    || peer.starts_with("[::1]:")
                    || peer == env.endpoint)
            })
            .collect();

        assert!(downloaded, "no data from any peer within 90 s");
        let (socks, tcp, utp) = peers.unwrap();
        assert_eq!((tcp, utp), (0, 0), "direct peer connections");
        assert!(!ours.is_empty(), "ss shows no sockets for this process");
        assert!(
            leaks.is_empty(),
            "sockets outside the VPN:\n{}",
            leaks.join("\n")
        );
        println!(
            "{} sockets checked, {} peers through the proxy",
            ours.len(),
            socks
        );
    }
}
