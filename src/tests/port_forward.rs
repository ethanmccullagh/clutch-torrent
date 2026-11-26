use super::socks5::{recv_relayed, udp_associate};
use super::*;
use crate::port_forward::start_port_forward;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Real inbound traffic: this machine connects to the VPN's public address from its own network
/// (outside the tunnel), and the traffic must come in through the port mapping.
/// Runs before the socks5 tests (name order), so the UDP association below is the first and binds
/// INBOUND_PORT.
#[tokio::test]
async fn inbound_tcp_and_udp() {
    let _tunnel = lock_tunnel().await;
    let control = start_test_environment().await;
    let client_listener = tokio::net::TcpListener::bind(("127.0.0.1", INBOUND_PORT))
        .await
        .expect("INBOUND_PORT is taken on this machine");
    let mapping = start_port_forward(control, INBOUND_PORT).await.unwrap();
    println!("mapping: {mapping}");

    // TCP: the client's listener gets the connection and its reply goes back out.
    let target = std::net::SocketAddr::from((mapping.public_ip, mapping.tcp_port));
    let outside = tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect_timeout(&target, Duration::from_secs(5))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.write_all(b"ping")?;
        let mut reply = [0u8; 4];
        stream.read_exact(&mut reply)?;
        Ok::<_, std::io::Error>(reply)
    });
    let (mut inbound, _) = tokio::time::timeout(Duration::from_secs(5), client_listener.accept())
        .await
        .expect("no incoming TCP connection was forwarded")
        .unwrap();
    let mut request = [0u8; 4];
    inbound.read_exact(&mut request).await.unwrap();
    assert_eq!(&request, b"ping");
    inbound.write_all(b"pong").await.unwrap();
    assert_eq!(&outside.await.unwrap().unwrap(), b"pong");

    // UDP: an unsolicited datagram arrives through the SOCKS5 UDP association, and a reply sent
    // back through the association reaches the sender.
    let (_association, relay) = udp_associate().await;
    let outside = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
    outside
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    outside
        .send_to(b"udp ping", (mapping.public_ip, mapping.udp_port))
        .unwrap();
    let (header, payload) = recv_relayed(&relay).await;
    assert_eq!(payload, b"udp ping");

    let mut reply = header.to_vec(); // same address: back to the sender
    reply.extend_from_slice(b"udp pong");
    relay.send(&reply).await.unwrap();
    let mut buf = [0u8; 64];
    let n = tokio::task::spawn_blocking(move || outside.recv(&mut buf).map(|n| buf[..n].to_vec()))
        .await
        .unwrap()
        .expect("UDP reply did not get back out");
    assert_eq!(n, b"udp pong");
}
