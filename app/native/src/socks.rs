use crate::schema::ProxyMessage;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

pub async fn socks_listen(listener: TcpListener) {
    loop {
        let (stream, _): (TcpStream, _) = listener.accept().await.unwrap();

        // This tx would go to your smoltcp engine
        let (engine_tx, _) = mpsc::channel(100);

        tokio::spawn(async move {
            if let Err(e) = handle_socks5_client(stream, engine_tx).await {
                eprintln!("Proxy error: {}", e);
            }
        });
    }
}

async fn handle_socks5_client(
    mut stream: TcpStream,
    engine_tx: mpsc::Sender<ProxyMessage>,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut buf = [0u8; 512];

    // --- 1. SOCKS5 Greeting ---
    stream.read_exact(&mut buf[..2]).await?; // [VER, NMETHODS]
    let nmethods = buf[1] as usize;
    stream.read_exact(&mut buf[..nmethods]).await?;
    stream.write_all(&[0x05, 0x00]).await?; // No Auth required

    // --- 2. Connection Request ---
    stream.read_exact(&mut buf[..4]).await?; // [VER, CMD, RSV, ATYP]
    if buf[1] != 0x01 {
        return Err("Only CONNECT supported".into());
    }

    // Read Destination (assuming IPv4 for simplicity)
    let mut ip = [0u8; 4];
    stream.read_exact(&mut ip).await?;
    let port = stream.read_u16().await?;

    // --- 3. Request smoltcp to Connect ---
    let (resp_tx, mut resp_rx) = mpsc::channel(1);
    engine_tx
        .send(ProxyMessage::Connect {
            addr: ip,
            port,
            resp: resp_tx,
        })
        .await?;

    // Wait for the smoltcp Engine to signal that the VPN Handshake is done
    if let Some(true) = resp_rx.recv().await {
        // Send SOCKS5 Success Response
        stream
            .write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
            .await?;
    } else {
        stream.write_all(&[0x05, 0x05]).await?; // Connection Refused
        return Ok(());
    }

    // --- 4. The Pump Loop ---
    let mut payload = [0u8; 4096];
    loop {
        let n = stream.read(&mut payload).await?;
        if n == 0 {
            break;
        }
        engine_tx
            .send(ProxyMessage::Data {
                data: payload[..n].to_vec(),
            })
            .await?;
    }

    engine_tx.send(ProxyMessage::Close).await?;
    Ok(())
}
