use ini::Ini;
use tokio::net::{TcpStream, TcpListener};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc};
pub mod helper;
pub mod setup;
pub mod test;
pub mod schema;

//mod test;
use setup::*;
use test::*;
use schema::ProxyMessage;

use crate::schema::VpnEngine;



#[tokio::main]
async fn main() {
    //load config file from path
    let conf = Ini::load_from_file("/home/ethanm/Downloads/cachy-CA-1149.conf").unwrap();

    let mut engine = setup_engine(conf).await;


    //let listener = TcpListener::bind("127.0.0.1:1080").await.unwrap();
    //TEST: ping google using smoltcp interface
    let _ = interface_test(&mut engine).await;

    //event_loop(&mut iface, &mut tunnel, &mut device, &endpoint);

    /* 

    loop {
        let (stream, _) = listener.accept().await.unwrap();
        
        // This tx would go to your smoltcp engine
        let (engine_tx, _) = mpsc::channel(100); 

        tokio::spawn(async move {
            if let Err(e) = handle_socks5_client(stream, engine_tx).await {
                eprintln!("Proxy error: {}", e);
            }
        });
    }

    */
}

async fn handle_socks5_client(
    mut stream: TcpStream, 
    engine_tx: mpsc::Sender<ProxyMessage>
) -> Result<(), Box<dyn std::error::Error>> {
    let mut buf = [0u8; 512];

    // --- 1. SOCKS5 Greeting ---
    stream.read_exact(&mut buf[..2]).await?; // [VER, NMETHODS]
    let nmethods = buf[1] as usize;
    stream.read_exact(&mut buf[..nmethods]).await?;
    stream.write_all(&[0x05, 0x00]).await?; // No Auth required

    // --- 2. Connection Request ---
    stream.read_exact(&mut buf[..4]).await?; // [VER, CMD, RSV, ATYP]
    if buf[1] != 0x01 { return Err("Only CONNECT supported".into()); }
    
    // Read Destination (assuming IPv4 for simplicity)
    let mut ip = [0u8; 4];
    stream.read_exact(&mut ip).await?;
    let port = stream.read_u16().await?;

    // --- 3. Request smoltcp to Connect ---
    let (resp_tx, mut resp_rx) = mpsc::channel(1);
    engine_tx.send(ProxyMessage::Connect { addr: ip, port, resp: resp_tx }).await?;

    // Wait for the smoltcp Engine to signal that the VPN Handshake is done
    if let Some(true) = resp_rx.recv().await {
        // Send SOCKS5 Success Response
        stream.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await?;
    } else {
        stream.write_all(&[0x05, 0x05]).await?; // Connection Refused
        return Ok(());
    }

    // --- 4. The Pump Loop ---
    let mut payload = [0u8; 4096];
    loop {
        let n = stream.read(&mut payload).await?;
        if n == 0 { break; }
        engine_tx.send(ProxyMessage::Data { data: payload[..n].to_vec() }).await?;
    }

    engine_tx.send(ProxyMessage::Close).await?;
    Ok(())
}

/*
async fn run_engine(mut vpn: VpnEngine, mut proxy_rx: mpsc::Receiver<ProxyMessage>) {
    let mut recv_buf = [0u8; 4096]; // Recv buffer
    let mut vpn_buf = [0u8; 4096]; // Encrypt/Decrypt scratch buffer
    loop {
        let now = Instant::now();

        // 1. Let smoltcp process its internal state
        // This returns a "poll_delay" — how long until the NEXT thing needs to happen
        vpn.iface.poll(now, &mut vpn.device, &mut vpn.sockets);
        
        let poll_delay = vpn.iface.poll_delay(now, &mut vpn.sockets)
            .map(|d| Duration::from_micros(d.total_micros()))
            .unwrap_or(Duration::from_millis(100)); // Default sleep if idle

        // 2. Wait for the NEXT event
        tokio::select! {
            // Wait for data from Libtransmission
            msg = proxy_rx.recv() => {
                if let Some(m) = msg {
                    handle_message(m, &mut vpn);
                }
            }
            
            // Wait for incoming UDP from ProtonVPN
            Ok((len, addr)) = vpn.udp_socket.recv_from(&mut recv_buf) => {
                let decrypted = tunn.decapsulate(None, &recv_buf[..len], &mut vpn_buf);
                // Push decrypted IP packet into smoltcp device
                vpn.device.rx_queue.push_back(vpn_buf[..decrypted_len].to_vec());
            }

            // Wait for smoltcp's next scheduled task (retransmission/timer)
            _ = tokio::time::sleep(poll_delay) => {
                // Just wake up and loop to trigger iface.poll()
            }
        }
    }
}

fn event_loop(
    iface: &mut Interface,
    tunnel: &mut Tunn,
    device: &mut MyVirtualDevice,
    endpoint: &SocketAddr,
) {
    let mut recv_buf = [0u8; 4096]; // Recv buffer
    let mut scratch_buf = [0u8; 4096]; // Encrypt/Decrypt scratch buffer

    let mut sockets = SocketSet::new(vec![]);

    let tcp_rx_buff = tcp::SocketBuffer::new(vec![0u8; 4096]);
    let tcp_tx_buff = tcp::SocketBuffer::new(vec![0u8; 4096]);
    let mut tcp_socket = tcp::Socket::new(tcp_rx_buff, tcp_tx_buff);
    tcp_socket.listen(8080).unwrap();
    let _tcp_handle = sockets.add(tcp_socket);

    let udp_socket = UdpSocket::bind("0.0.0.0:0");
    udp_socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();

    println!("RUNNING MAIN EVENT LOOP");

    loop {
        let now = Instant::now();

        // 1. Let smoltcp do its work (Process TCP/IP stack)
        // This looks at rx_queue and fills tx_queue if needed
        iface.poll(now, device, &mut sockets);

        // --- OUTBOUND PATH (App -> VPN) ---
        // Take raw IP packets from smoltcp...
        while let Some(ip_packet) = device.tx_queue.pop_front() {
            // ...and feed them to BoringTun for encryption
            println!(">SMOLTCP Packet recieved in transmit queue");
            match tunnel.encapsulate(&ip_packet, &mut scratch_buf) {
                TunnResult::WriteToNetwork(encrypted_bytes) => {
                    // Send wireguard packet to the real internet
                    udp_socket.send_to(encrypted_bytes, &endpoint).unwrap();
                    println!(">BORINGTUN Encrypted packet write to network");
                }
                _ => {}
            }
        }

        // --- INBOUND PATH (VPN -> App) ---
        // Read encrypted UDP packets from the real internet...
        if let Ok((len, _)) = udp_socket.recv_from(&mut recv_buf) {
            println!("<UDP SOCKET Packet recieved");
            // ...and feed them to BoringTun for decryption
            match tunnel.decapsulate(None, &recv_buf[..len], &mut scratch_buf) {
                TunnResult::WriteToTunnelV4(decrypted_bytes, _) => {
                    // Take the raw IP packet and put it in smoltcp's mailbox
                    device.rx_queue.push_back(decrypted_bytes.to_vec());
                    println!("<BORINGTUN Encrypted packet write to tunnelV4");
                }
                // Handle Handshakes/Keepalives (WriteToNetwork)
                TunnResult::WriteToNetwork(b) => {
                    udp_socket.send_to(b, endpoint).unwrap();
                    println!("<BORINGTUN Encrypted packet write to network");
                }
                _ => {
                    println!("<BORINGTUN Encrypted packet done/other");
                }
            }
        }
    }
} */
