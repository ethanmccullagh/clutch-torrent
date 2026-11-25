use native::{bind_socks, Bridge, DEFAULT_SOCKS_PORT};

#[tokio::main]
async fn main() {
    // Usage: native [config.conf] [--forward <client listen port>] [--socks-port <port>]
    let mut config_path = "cachy3-CA-1208.conf".to_string();
    let mut forward_port = None;
    let mut socks_port = DEFAULT_SOCKS_PORT;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--forward" {
            let port = args.next().and_then(|p| p.parse::<u16>().ok());
            forward_port = Some(port.expect("--forward needs the client's listen port"));
        } else if arg == "--socks-port" {
            let port = args.next().and_then(|p| p.parse::<u16>().ok());
            socks_port = port.expect("--socks-port needs a port number");
        } else {
            config_path = arg;
        }
    }

    let socks = bind_socks(socks_port).await.unwrap_or_else(|e| {
        eprintln!("{e:#}");
        std::process::exit(1);
    });
    let socks_addr = socks.local_addr().expect("bound listener has an address");
    let mut bridge = match Bridge::start(&config_path, forward_port, socks).await {
        Ok(bridge) => bridge,
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
    };
    println!("SOCKS5 server listening on {socks_addr}");
    if let (Some(port), Some(mapping)) = (forward_port, bridge.mapping) {
        println!(
            "Incoming connections forwarded to 127.0.0.1:{port}. Set the client's listen port \
             to {port} and its announced port to {}: {mapping}",
            mapping.tcp_port
        );
    }
    eprintln!("{} stopped", bridge.wait().await);
}
