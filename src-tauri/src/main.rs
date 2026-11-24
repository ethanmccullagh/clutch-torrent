// No extra console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use native::{Bridge, SOCKS_ADDR};
use serde::Serialize;
use tauri::async_runtime::Mutex;
use tauri::State;

#[derive(Default)]
struct AppState {
    bridge: Mutex<Option<Bridge>>,
}

/// What the frontend shows; sent back by every command.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Status {
    running: bool,
    socks_addr: &'static str,
    /// Public address and ports to announce, when port forwarding is on.
    public_ip: Option<String>,
    public_tcp_port: Option<u16>,
    public_udp_port: Option<u16>,
}

fn status(bridge: &Option<Bridge>) -> Status {
    let mapping = bridge.as_ref().and_then(|b| b.mapping);
    Status {
        running: bridge.is_some(),
        socks_addr: SOCKS_ADDR,
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
    let mut bridge = state.bridge.lock().await;
    if bridge.is_none() {
        // A malformed config panics in setup.rs; in its own task that becomes an error, not a crash.
        let started =
            tauri::async_runtime::spawn(async move { Bridge::start(&config_path, forward_port).await })
                .await
                .map_err(|_| "invalid WireGuard config (setup panicked, see the log)".to_string())?
                .map_err(|e| format!("{e:#}"))?;
        *bridge = Some(started);
    }
    Ok(status(&bridge))
}

#[tauri::command]
async fn stop_bridge(state: State<'_, AppState>) -> Result<Status, String> {
    let mut bridge = state.bridge.lock().await;
    if let Some(running) = bridge.take() {
        running.stop().await;
    }
    Ok(status(&bridge))
}

#[tauri::command]
async fn bridge_status(state: State<'_, AppState>) -> Result<Status, String> {
    Ok(status(&*state.bridge.lock().await))
}

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .invoke_handler(tauri::generate_handler![start_bridge, stop_bridge, bridge_status])
        .run(tauri::generate_context!())
        .expect("error while running the Tauri app");
}
