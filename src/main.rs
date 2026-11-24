use native::{Bridge, SOCKS_ADDR};

#[tokio::main]
async fn main() {
    // Usage: native [config.conf] [--forward <client listen port>]
    let mut config_path = "cachy3-CA-1208.conf".to_string();
    let mut forward_port = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--forward" {
            let port = args.next().and_then(|p| p.parse::<u16>().ok());
            forward_port = Some(port.expect("--forward needs the client's listen port"));
        } else {
            config_path = arg;
        }
    }

    let mut bridge = match Bridge::start(&config_path, forward_port).await {
        Ok(bridge) => bridge,
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
    };
    println!("SOCKS5 server listening on {SOCKS_ADDR}");
    if let (Some(port), Some(mapping)) = (forward_port, bridge.mapping) {
        println!(
            "Incoming connections forwarded to 127.0.0.1:{port}. Set the client's listen port \
             to {port} and its announced port to {}: {mapping}",
            mapping.tcp_port
        );
    }
    eprintln!("{} stopped", bridge.wait().await);
}
