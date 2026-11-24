use ini::Ini;
use tokio::sync::mpsc;

//import other files
pub mod boringthreads;
pub mod event_driven_engine;
pub mod helper;
pub mod port_forward;
pub mod schema;
pub mod setup;
pub mod socks5;
#[cfg(test)]
mod tests;

use boringthreads::spawn_boring_threads;
use event_driven_engine::run_engine;
use setup::*;

use crate::schema::ProxyMessage;

#[tokio::main]
async fn main() {
    // Usage: native [config.conf] [--forward <client listen port>]
    let mut config_path = "cachy3-CA-1208.conf".to_string();
    let mut forward_port = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--forward" {
            let port = args.next().and_then(|p| p.parse::<u16>().ok());
            forward_port = Some(port.expect("--forward needs the client's listen port"));
        } else {
            config_path = arg;
        }
    }
    let conf = Ini::load_from_file(&config_path)
        .unwrap_or_else(|e| panic!("failed to load WireGuard config {config_path}: {e}"));

    //Communication channels for sync engine <--> async tasks
    let (vpn_sender, vpn_receiver) = mpsc::unbounded_channel::<Vec<u8>>();
    let (device_sender, device_receiver) = mpsc::channel::<Vec<u8>>(100);
    let (control_sender, control_receiver) = mpsc::channel::<ProxyMessage>(100);

    //setup and start vpn tunnel tasks
    let (tunn, endpoint) = setup_tunnel(conf.clone());
    let _boring_handles = spawn_boring_threads(tunn, endpoint, vpn_receiver, device_sender)
        .await
        .expect("failed to bind UDP socket for the WireGuard tunnel");

    let mut engine = tokio::spawn(run_engine(
        conf,
        device_receiver,
        vpn_sender,
        control_receiver,
    ));

    if let Some(port) = forward_port {
        match port_forward::start_port_forward(control_sender.clone(), port).await {
            Ok(mapping) => println!(
                "Incoming connections forwarded to 127.0.0.1:{port}. Set the client's listen port \
                 to {port} and its announced port to {}: {mapping}",
                mapping.tcp_port
            ),
            Err(e) => eprintln!("port forwarding unavailable: {e:#}"),
        }
    }

    tokio::select! {
        _ = &mut engine => eprintln!("engine stopped"),
        result = socks5::run_server(control_sender, forward_port) => {
            if let Err(e) = result {
                eprintln!("SOCKS5 server failed: {e}");
            }
        }
    }
}
