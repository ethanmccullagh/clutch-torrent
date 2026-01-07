use crate::schema::ChannelMessage;
use core::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

pub async fn socks_spawn(listener: TcpListener) {
    let (stream, addr): (TcpStream, SocketAddr) = listener.accept().await.unwrap();

    // This tx would go to your smoltcp engine
    let (socks_tx, socks_rx) = mpsc::channel::<ChannelMessage>(100);
    let (engine_tx, engine_rx) = mpsc::channel::<ChannelMessage>(100);
}
