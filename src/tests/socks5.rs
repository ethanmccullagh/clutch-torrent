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

/// Write `payload` through the proxied stream and check the echo server sends it back.
async fn assert_echo(stream: &mut TcpStream, payload: &[u8]) {
    let (mut reader, mut writer) = stream.split();
    let mut echoed = vec![0u8; payload.len()];
    let (write, read) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(writer.write_all(payload), reader.read_exact(&mut echoed))
    })
    .await
    .expect("SOCKS5 echo timed out");
    write.unwrap();
    read.unwrap();
    assert!(echoed == payload, "SOCKS5 echo mismatch");
}

/// 256 KB round trip through the echo server: several times the engine's 64 KB socket
/// buffers, so it exercises queued writes and read backpressure.
#[tokio::test]
async fn echo_256kb() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let mut stream = handshake().await;

    // CONNECT 45.79.112.203:4242
    let code = request(&mut stream, &[5, 1, 0, 1, 45, 79, 112, 203, 0x10, 0x92]).await;
    assert_eq!(code, 0, "SOCKS5 CONNECT failed");

    // 1 KB lines: the echo server is line based.
    let payload: Vec<u8> = (0..256 * 1024u32)
        .map(|i| {
            if i % 1024 == 1023 {
                b'\n'
            } else {
                b'a' + (i % 26) as u8
            }
        })
        .collect();
    assert_echo(&mut stream, &payload).await;
}

/// The hostname is resolved through the tunnel, then the connection works end to end.
#[tokio::test]
async fn connect_by_domain() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let mut stream = handshake().await;

    let code = request(&mut stream, &domain_connect("tcpbin.com", 4242)).await;
    assert_eq!(code, 0, "SOCKS5 CONNECT failed");
    assert_echo(&mut stream, b"hello by name\n").await;
}

/// The server closes after its HTTP/1.0 response; the client must see that EOF.
#[tokio::test]
async fn remote_close_reaches_client() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let mut stream = handshake().await;

    let code = request(&mut stream, &domain_connect("example.com", 80)).await;
    assert_eq!(code, 0, "SOCKS5 CONNECT failed");
    stream
        .write_all(b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n")
        .await
        .unwrap();

    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
        .await
        .expect("no EOF after the server closed")
        .unwrap();
    assert!(response.starts_with(b"HTTP/1."), "not an HTTP response");
}

#[tokio::test]
async fn ipv6_is_unsupported() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let mut stream = handshake().await;

    let mut req = vec![5, 1, 0, 4];
    req.extend_from_slice(&std::net::Ipv6Addr::LOCALHOST.octets());
    req.extend_from_slice(&80u16.to_be_bytes());
    let code = request(&mut stream, &req).await;
    assert_eq!(code, 8, "expected 'Address type not supported'");
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
