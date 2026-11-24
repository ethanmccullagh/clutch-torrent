use crate::helper::decode_key;
use boringtun::noise::Tunn;
use boringtun::x25519::{PublicKey, StaticSecret};
use core::net::SocketAddr;
use ini::Ini;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, Tracer};
use smoltcp::wire::{HardwareAddress, IpCidr, Ipv4Address};

pub fn setup_engine<'a, D: Device>(conf: Ini, device: D) -> (Interface, Tracer<D>, SocketSet<'a>) {
    let config = Config::new(HardwareAddress::Ip);

    let mut tracer = Tracer::new(device, |timestamp, packet| {
        let text = packet.to_string();
        // Packet's direction field is private; smoltcp prefixes Tx with "-> " and Rx with "<- ".
        let color = if text.starts_with("->") { "31" } else { "32" }; // red out, green in
        println!("\x1b[{color}m[{timestamp}] {}\x1b[0m", text.trim_end());
    });

    let vpn_ip: IpCidr = conf
        .get_from(Some("Interface"), "Address")
        .expect("config is missing [Interface] Address")
        .parse::<IpCidr>()
        .expect("[Interface] Address is not a valid CIDR (e.g. 10.2.0.2/32)");

    let gateway_ip: Ipv4Address = conf
        .get_from(Some("Interface"), "DNS")
        .expect("config is missing [Interface] DNS (used as the gateway)")
        .parse::<Ipv4Address>()
        .expect("[Interface] DNS is not a single IPv4 address");

    let mut iface = Interface::new(config, &mut tracer, smoltcp::time::Instant::now());

    iface.update_ip_addrs(|addrs| {
        addrs
            .push(IpCidr::new(vpn_ip.address(), 32))
            .expect("interface address list is full");
    });

    iface
        .routes_mut()
        .add_default_ipv4_route(gateway_ip)
        .expect("routing table is full, cannot add default route");

    iface.set_any_ip(true);

    let sockets = SocketSet::new(vec![]);

    (iface, tracer, sockets)
}

pub fn setup_tunnel(conf: Ini) -> (Tunn, SocketAddr) {
    let private_b64 = conf
        .get_from(Some("Interface"), "PrivateKey")
        .expect("config is missing [Interface] PrivateKey");
    let public_b64 = conf
        .get_from(Some("Peer"), "PublicKey")
        .expect("config is missing [Peer] PublicKey");
    let endpoint: SocketAddr = conf
        .get_from(Some("Peer"), "Endpoint")
        .expect("config is missing [Peer] Endpoint")
        .parse()
        .expect("[Peer] Endpoint is not an ip:port address");

    let private_key = decode_key(private_b64).expect("invalid [Interface] PrivateKey");
    let public_key = decode_key(public_b64).expect("invalid [Peer] PublicKey");

    let tunnel = Tunn::new(
        StaticSecret::from(private_key),
        PublicKey::from(public_key),
        None,
        Some(25),
        1,
        None,
    )
    .expect("failed to create WireGuard tunnel");
    (tunnel, endpoint)
}
