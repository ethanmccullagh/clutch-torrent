use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Connect to the proxy and complete the no-auth handshake.
async fn handshake() -> TcpStream {
    let mut stream = TcpStream::connect("127.0.0.1:1080")
        .await
        .expect("Failed to connect to SOCKS5 proxy");
    stream.write_all(&[5, 1, 0]).await.unwrap();
    let mut resp = [0u8; 2];
    stream.read_exact(&mut resp).await.unwrap();
    assert_eq!(resp, [5, 0], "SOCKS5 handshake");
    stream
}

/// Send a request (`[VER, CMD, RSV, ATYP, addr.., port]`) and return the reply code.
async fn request(stream: &mut TcpStream, request: &[u8]) -> u8 {
    stream.write_all(request).await.unwrap();
    let mut reply = [0u8; 10];
    stream
        .read_exact(&mut reply)
        .await
        .expect("no SOCKS5 reply");
    assert_eq!(reply[0], 5, "SOCKS5 reply version");
    reply[1]
}

fn domain_connect(domain: &str, port: u16) -> Vec<u8> {
    let mut req = vec![5, 1, 0, 3, domain.len() as u8];
    req.extend_from_slice(domain.as_bytes());
    req.extend_from_slice(&port.to_be_bytes());
    req
}

/// 4 KB round trip through the echo server: many times the engine's 256-byte socket
/// buffers, so it exercises queued writes and read backpressure.
#[tokio::test]
async fn echo_4kb() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let mut stream = handshake().await;

    // CONNECT 45.79.112.203:4242
    let code = request(&mut stream, &[5, 1, 0, 1, 45, 79, 112, 203, 0x10, 0x92]).await;
    assert_eq!(code, 0, "SOCKS5 CONNECT failed");

    let mut payload: Vec<u8> = (0..4095u32).map(|i| b'a' + (i % 26) as u8).collect();
    payload.push(b'\n');
    stream.write_all(&payload).await.unwrap();

    let mut echoed = vec![0u8; payload.len()];
    tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut echoed))
        .await
        .expect("SOCKS5 echo timed out")
        .unwrap();
    assert!(echoed == payload, "SOCKS5 echo mismatch");
}

#[tokio::test]
#[ignore = "bug: socks5.rs connects to the first resolved address; an IPv6 one panics smoltcp (no IPv6 source address)"]
async fn connect_by_domain() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let mut stream = handshake().await;

    let code = request(&mut stream, &domain_connect("www.google.com", 80)).await;
    assert_eq!(code, 0, "SOCKS5 CONNECT failed");
}

#[tokio::test]
async fn bind_is_unsupported() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let mut stream = handshake().await;

    let code = request(&mut stream, &[5, 2, 0, 1, 0, 0, 0, 0, 0, 0]).await;
    assert_eq!(code, 7, "expected 'Command not supported'");
}

#[tokio::test]
#[ignore = "bug: socks5.rs closes the connection on DNS failure instead of replying 4"]
async fn unresolvable_domain() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let mut stream = handshake().await;

    let code = request(
        &mut stream,
        &domain_connect("unresolvable.domain.for.test", 80),
    )
    .await;
    assert_eq!(code, 4, "expected 'Host unreachable'");
}
