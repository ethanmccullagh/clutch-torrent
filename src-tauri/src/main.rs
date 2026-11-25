// No extra console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use native::{bind_socks, refuse_connections, Bridge, DEFAULT_SOCKS_PORT};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tauri::async_runtime::{JoinHandle, Mutex};
use tauri::{Manager, State};
use tokio::net::TcpListener;

/// Saved in `<app config dir>/settings.json`, read once at startup.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    /// The bridge's SOCKS5 port on 127.0.0.1.
    proxy_port: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            proxy_port: DEFAULT_SOCKS_PORT,
        }
    }
}

impl Settings {
    fn load(path: &PathBuf) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default(); // first run
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            eprintln!("ignoring unreadable {}: {e}", path.display());
            Self::default()
        })
    }
}

struct AppState {
    settings_path: PathBuf,
    /// The SOCKS5 listener, bound at startup and held until the app exits, or why it couldn't be
    /// bound. While the bridge is stopped, `refuse_connections` keeps the port taken, so no other
    /// program can receive traffic meant for the proxy.
    socks: Result<Arc<TcpListener>, String>,
    bridge: Mutex<BridgeSlot>,
}

/// Exactly one of these accepts on the SOCKS5 listener at a time.
#[derive(Default)]
struct BridgeSlot {
    running: Option<Bridge>,
    refuser: Option<JoinHandle<()>>,
}

impl BridgeSlot {
    fn refuse(&mut self, socks: &Arc<TcpListener>) {
        self.refuser = Some(tauri::async_runtime::spawn(refuse_connections(
            socks.clone(),
        )));
    }
}

/// What the frontend shows; sent back by every bridge command.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    running: bool,
    /// The held proxy address, or `None` if the port couldn't be bound (see `proxy_error`).
    socks_addr: Option<String>,
    proxy_error: Option<String>,
    /// Public address and ports to announce, when port forwarding is on.
    public_ip: Option<String>,
    public_tcp_port: Option<u16>,
    public_udp_port: Option<u16>,
}

fn socks_port(state: &AppState) -> Option<u16> {
    let listener = state.socks.as_ref().ok()?;
    Some(listener.local_addr().ok()?.port())
}

fn status(state: &AppState, slot: &BridgeSlot) -> Status {
    let mapping = slot.running.as_ref().and_then(|b| b.mapping);
    Status {
        running: slot.running.is_some(),
        socks_addr: socks_port(state).map(|port| format!("127.0.0.1:{port}")),
        proxy_error: state.socks.as_ref().err().cloned(),
        public_ip: mapping.map(|m| m.public_ip.to_string()),
        public_tcp_port: mapping.map(|m| m.tcp_port),
        public_udp_port: mapping.map(|m| m.udp_port),
    }
}

#[tauri::command]
async fn start_bridge(
    state: State<'_, AppState>,
    config_path: String,
    forward_port: Option<u16>,
) -> Result<Status, String> {
    let socks = state.socks.clone()?;
    let mut slot = state.bridge.lock().await;
    if slot.running.is_none() {
        if let Some(refuser) = slot.refuser.take() {
            refuser.abort(); // the bridge's SOCKS5 server takes over the listener
        }
        // A malformed config panics in setup.rs; in its own task that becomes an error, not a crash.
        let listener = socks.clone();
        let started = tauri::async_runtime::spawn(async move {
            Bridge::start(&config_path, forward_port, listener).await
        })
        .await
        .map_err(|_| "invalid WireGuard config (setup panicked, see the log)".to_string())
        .and_then(|r| r.map_err(|e| format!("{e:#}")));
        match started {
            Ok(bridge) => slot.running = Some(bridge),
            Err(e) => {
                slot.refuse(&socks);
                return Err(e);
            }
        }
    }
    Ok(status(&state, &slot))
}

#[tauri::command]
async fn stop_bridge(state: State<'_, AppState>) -> Result<Status, String> {
    let mut slot = state.bridge.lock().await;
    if let Some(running) = slot.running.take() {
        running.stop().await;
        if let Ok(socks) = &state.socks {
            slot.refuse(socks);
        }
    }
    Ok(status(&state, &slot))
}

#[tauri::command]
async fn bridge_status(state: State<'_, AppState>) -> Result<Status, String> {
    let slot = state.bridge.lock().await;
    Ok(status(&state, &slot))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SettingsView {
    /// Saved port; used from the next start.
    proxy_port: u16,
    /// Port in use now, if it could be bound.
    active_proxy_port: Option<u16>,
}

fn settings_view(state: &AppState) -> SettingsView {
    SettingsView {
        proxy_port: Settings::load(&state.settings_path).proxy_port,
        active_proxy_port: socks_port(state),
    }
}

#[tauri::command]
fn get_settings(state: State<'_, AppState>) -> SettingsView {
    settings_view(&state)
}

/// Save the proxy port. It takes effect at the next start, since the port is held from startup.
#[tauri::command]
fn set_proxy_port(state: State<'_, AppState>, port: u16) -> Result<SettingsView, String> {
    if port < 1024 {
        return Err("use a port from 1024 to 65535 (lower ones need root)".into());
    }
    let text =
        serde_json::to_string_pretty(&Settings { proxy_port: port }).map_err(|e| e.to_string())?;
    if let Some(dir) = state.settings_path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("couldn't save settings: {e}"))?;
    }
    std::fs::write(&state.settings_path, text)
        .map_err(|e| format!("couldn't save settings: {e}"))?;
    Ok(settings_view(&state))
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let settings = Settings::load(&settings_path);

            let socks = tauri::async_runtime::block_on(bind_socks(settings.proxy_port))
                .map_err(|e| format!("{e:#}"));
            let mut slot = BridgeSlot::default();
            match &socks {
                Ok(listener) => slot.refuse(listener), // hold the port until the bridge starts
                Err(e) => eprintln!("proxy port unavailable: {e}"),
            }
            app.manage(AppState {
                settings_path,
                socks,
                bridge: Mutex::new(slot),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            start_bridge,
            stop_bridge,
            bridge_status,
            get_settings,
            set_proxy_port
        ])
        .run(tauri::generate_context!())
        .expect("error while running the Tauri app");
}
