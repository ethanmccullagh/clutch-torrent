use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Connect to the proxy and complete the no-auth handshake.
pub(super) async fn handshake() -> TcpStream {
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

/// Open a UDP association; return the control stream (closing it ends the association) and a UDP
/// socket connected to the relay.
pub(super) async fn udp_associate() -> (TcpStream, tokio::net::UdpSocket) {
    // Declare our port, so datagrams that arrive before we send anything still reach us.
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let [hi, lo] = socket.local_addr().unwrap().port().to_be_bytes();
    let mut stream = handshake().await;
    stream
        .write_all(&[5, 3, 0, 1, 0, 0, 0, 0, hi, lo])
        .await
        .unwrap();
    let mut reply = [0u8; 10];
    stream
        .read_exact(&mut reply)
        .await
        .expect("no SOCKS5 reply");
    assert_eq!(reply[..2], [5, 0], "UDP ASSOCIATE failed");
    let relay = std::net::SocketAddr::from((
        [reply[4], reply[5], reply[6], reply[7]],
        u16::from_be_bytes([reply[8], reply[9]]),
    ));
    socket.connect(relay).await.unwrap();
    (stream, socket)
}

/// A DNS query for example.com A with transaction id `id`.
fn dns_query(id: u16) -> Vec<u8> {
    let mut query = id.to_be_bytes().to_vec();
    query.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    query.extend_from_slice(b"\x07example\x03com\x00");
    query.extend_from_slice(&[0, 1, 0, 1]);
    query
}

/// Receive one relayed datagram; return its SOCKS5 header (10 bytes, IPv4) and payload.
pub(super) async fn recv_relayed(socket: &tokio::net::UdpSocket) -> ([u8; 10], Vec<u8>) {
    let mut buf = [0u8; 2048];
    let n = tokio::time::timeout(Duration::from_secs(5), socket.recv(&mut buf))
        .await
        .expect("no datagram from the relay")
        .unwrap();
    (buf[..10].try_into().unwrap(), buf[10..n].to_vec())
}

#[tokio::test]
async fn udp_associate_dns() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let (_control, socket) = udp_associate().await;

    let mut datagram = vec![0, 0, 0, 1, 8, 8, 8, 8, 0, 53];
    datagram.extend_from_slice(&dns_query(0xBEEF));
    socket.send(&datagram).await.unwrap();

    let (header, reply) = recv_relayed(&socket).await;
    assert_eq!(
        header,
        [0, 0, 0, 1, 8, 8, 8, 8, 0, 53],
        "reply header must name the sender"
    );
    assert_eq!(reply[..2], [0xBE, 0xEF], "reply id mismatch");
}

/// Replies that arrive together must all get through (DHT traffic is bursty).
#[tokio::test]
async fn udp_associate_burst() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let (_control, socket) = udp_associate().await;

    for id in 0..20u16 {
        let mut datagram = vec![0, 0, 0, 1, 8, 8, 8, 8, 0, 53];
        datagram.extend_from_slice(&dns_query(id));
        socket.send(&datagram).await.unwrap();
    }
    let mut seen = std::collections::HashSet::new();
    while seen.len() < 20 {
        let (_, reply) = recv_relayed(&socket).await;
        seen.insert(u16::from_be_bytes([reply[0], reply[1]]));
    }
}

/// A datagram addressed by hostname is resolved through the tunnel.
#[tokio::test]
async fn udp_associate_domain_target() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let (_control, socket) = udp_associate().await;

    let mut datagram = vec![0, 0, 0, 3, 10];
    datagram.extend_from_slice(b"dns.google");
    datagram.extend_from_slice(&53u16.to_be_bytes());
    datagram.extend_from_slice(&dns_query(0xCAFE));
    socket.send(&datagram).await.unwrap();

    let (header, reply) = recv_relayed(&socket).await;
    assert_eq!(
        header[3], 1,
        "reply header should carry the resolved IPv4 address"
    );
    assert_eq!(reply[..2], [0xCA, 0xFE], "reply id mismatch");
}

/// Closing the control connection ends the association and closes the relay socket.
#[tokio::test]
async fn udp_association_ends_with_control_connection() {
    let _tunnel = lock_tunnel().await;
    test_env().await; // starts the shared SOCKS5 server
    let (control, socket) = udp_associate().await;
    drop(control);
    tokio::time::sleep(Duration::from_millis(200)).await;

    // A closed local UDP port answers with ICMP port unreachable, reported on the next recv.
    // Blocking std socket: tokio's recv isn't woken by a socket error alone.
    let socket = socket.into_std().unwrap();
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket.send(&[0, 0, 0, 1, 8, 8, 8, 8, 0, 53]).unwrap();
    let result = socket.recv(&mut [0u8; 64]);
    assert!(
        matches!(&result, Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused),
        "relay still open after the control connection closed: {result:?}"
    );
}
