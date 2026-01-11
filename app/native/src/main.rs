use ini::Ini;
use tokio::time::{sleep, Duration};


//import other files
pub mod engine;
pub mod helper;
pub mod schema;
pub mod setup;
pub mod socks5;
pub mod test;
pub mod boringthreads;

//mod test;
use setup::*;
use test::*;
use boringthreads::spawn_boring_threads;

#[tokio::main]
async fn main() {
    //load config file from path
    let conf = Ini::load_from_file("/home/ethanm/Downloads/cachy-CA-1149.conf").unwrap();

    let (mut engine, control_sender, vpn_receiver, local_receiver, device_sender) = setup_engine(conf.clone());

    let (tunn, endpoint) = setup_tunnel(conf);
    let _task_handles = spawn_boring_threads(tunn, endpoint, vpn_receiver, device_sender).await.unwrap();


    ping_test(&mut engine);

    //let _joinhandle = tokio::spawn(connection_test(control_sender));

}
