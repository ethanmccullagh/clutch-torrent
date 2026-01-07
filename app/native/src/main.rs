use ini::Ini;


//import other files
pub mod engine;
pub mod helper;
pub mod schema;
pub mod setup;
pub mod socks5;
pub mod test;
pub mod udpthreads;

//mod test;
use setup::*;
use test::*;
use udpthreads::spawn_udp_threads;

#[tokio::main]
async fn main() {
    //load config file from path
    let conf = Ini::load_from_file("/home/ethanm/Downloads/cachy-CA-1149.conf").unwrap();

    let (udp_transmit_s, udp_recv_r, _task_handles) = spawn_udp_threads().await.unwrap();

    let (mut engine, control_sender) = setup_engine(conf, udp_transmit_s, udp_recv_r).await;

    // let listener = TcpListener::bind("127.0.0.1:1080").await;

    //TEST: ping google using smoltcp interface

    loop {
        engine.process_packets();

        engine.send_to_vpn();

        engine.recv_from_vpn();
    }

}
