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

    let (tunn, endpoint) = setup_tunnel(conf.clone());

    let (udp_transmit_s, udp_recv_r, _task_handles) = spawn_boring_threads(tunn, endpoint).await.unwrap();

    let (mut engine, control_sender) = setup_engine(conf, udp_transmit_s, udp_recv_r).await;


    //TEST: ping google using smoltcp interface
    ping_test(&mut engine);

    //let _joinhandle = tokio::spawn(connection_test(control_sender));

}
