#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use std::time::Duration;

    async fn socks5_handshake(stream: &mut TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        // SOCKS5 handshake
        stream.write_all(&[5, 1, 0]).await?;
        let mut resp = [0u8; 2];
        stream.read_exact(&mut resp).await?;

        if resp[0] != 5 || resp[1] != 0 {
            return Err("SOCKS5 handshake failed".into());
        }
        Ok(())
    }

use std::time::Duration;

    #[tokio::test]
    async fn test_socks5_connect_ipv4() -> Result<(), Box<dyn std::error::Error>> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut stream = TcpStream::connect("127.0.0.1:1080").await?;
        socks5_handshake(&mut stream).await?;

        // CONNECT to 8.8.8.8:80
        stream
            .write_all(&[
                5, 1, 0, 1, // VER, CMD=CONNECT, RSV, ATYP=IPv4
                8, 8, 8, 8, // IP: 8.8.8.8
                0, 80, // Port: 80
            ])
            .await?;

        let mut connect_resp = [0u8; 10];
        stream.read_exact(&mut connect_resp).await?;

        if connect_resp[0] != 5 || connect_resp[1] != 0 {
            return Err(format!("SOCKS5 CONNECT failed: {}", connect_resp[1]).into());
        }

        // Send HTTP request
        stream
            .write_all(b"GET / HTTP/1.0\r\nHost: 8.8.8.8\r\n\r\n")
            .await?;

        // Read response
        let mut buffer = [0u8; 1024];
        match tokio::time::timeout(
            Duration::from_secs(5),
            stream.read(&mut buffer),
        )
        .await
        {
            Ok(Ok(n)) if n > 0 => {
                println!("TEST: Received {} bytes response", n);
                Ok(())
            }
            Ok(Ok(_)) => Err("Connection closed".into()),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => Err("Timeout waiting for response".into()),
        }
    }

    #[tokio::test]
    async fn test_socks5_connect_domain() -> Result<(), Box<dyn std::error::Error>> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut stream = TcpStream::connect("127.0.0.1:1080").await?;
        socks5_handshake(&mut stream).await?;

        let domain = "www.google.com";
        let domain_len = domain.len() as u8;

        let mut request = vec![
            5, 1, 0, 3, // VER, CMD=CONNECT, RSV, ATYP=DOMAIN
        ];
        request.push(domain_len);
        request.extend_from_slice(domain.as_bytes());
        request.extend_from_slice(&[0, 80]); // Port 80

        stream.write_all(&request).await?;

        let mut connect_resp = [0u8; 10];
        stream.read_exact(&mut connect_resp).await?;

        if connect_resp[0] != 5 || connect_resp[1] != 0 {
            return Err(format!("SOCKS5 CONNECT failed: {}", connect_resp[1]).into());
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_socks5_unsupported_command() -> Result<(), Box<dyn std::error::Error>> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut stream = TcpStream::connect("127.0.0.1:1080").await?;
        socks5_handshake(&mut stream).await?;

        // BIND command (unsupported)
        stream
            .write_all(&[
                5, 2, 0, 1, // VER, CMD=BIND, RSV, ATYP=IPv4
                0, 0, 0, 0, // IP
                0, 0, // Port
            ])
            .await?;

        let mut resp = [0u8; 10];
        stream.read_exact(&mut resp).await?;

        if resp[1] != 7 {
            return Err(format!("Expected 'Command not supported' (7), got {}", resp[1]).into());
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_socks5_unresolvable_domain() -> Result<(), Box<dyn std::error::Error>> {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut stream = TcpStream::connect("127.0.0.1:1080").await?;
        socks5_handshake(&mut stream).await?;

        let domain = "unresolvable.domain.for.test";
        let domain_len = domain.len() as u8;

        let mut request = vec![
            5, 1, 0, 3, // VER, CMD=CONNECT, RSV, ATYP=DOMAIN
        ];
        request.push(domain_len);
        request.extend_from_slice(domain.as_bytes());
        request.extend_from_slice(&[0, 80]);

        stream.write_all(&request).await?;

        let mut resp = [0u8; 10];
        stream.read_exact(&mut resp).await?;

        if resp[1] != 4 { // Host unreachable
            return Err(format!("Expected 'Host unreachable' (4), got {}", resp[1]).into());
        }

        Ok(())
    }
}
