use ini::Ini;
use tokio::sync::mpsc;

//import other files
pub mod boringthreads;
pub mod engine;
pub mod helper;
pub mod schema;
pub mod setup;
pub mod socks5;
pub mod test;

//mod test;
use boringthreads::spawn_boring_threads;
use engine::run_engine;
use setup::*;

use crate::schema::ProxyMessage;

#[tokio::main]
async fn main() {
    //load config file from path
    let conf = Ini::load_from_file("/home/ethanm/Downloads/cachy3-CA-1208.conf").unwrap();

    //Communication channels for sync engine <--> async tasks
    let (vpn_sender, vpn_receiver) = mpsc::channel::<Vec<u8>>(100);
    let (device_sender, device_receiver) = mpsc::channel::<Vec<u8>>(100);
    let (_control_sender, control_receiver) = mpsc::channel::<ProxyMessage>(100);

    //setup and start vpn tunnel tasks
    let (tunn, endpoint) = setup_tunnel(conf.clone());
    let _boring_handles = spawn_boring_threads(tunn, endpoint, vpn_receiver, device_sender.clone())
        .await
        .unwrap();

    run_engine(conf, device_receiver, vpn_sender, control_receiver);
}

