use crate::helper::decode_key;
use boringtun::noise::Tunn;
use boringtun::x25519::{PublicKey, StaticSecret};
use core::net::SocketAddr;
use ini::Ini;
use smoltcp::iface::{Config, Interface, SocketSet};
use smoltcp::phy::{Device, Tracer};
use smoltcp::wire::{HardwareAddress, IpCidr, Ipv4Address, Ipv4Cidr};
use std::str::FromStr;
use std::sync::LazyLock;

pub fn setup_engine<'a, D: Device>(conf: Ini, device: D) -> (Interface, Tracer<D>, SocketSet<'a>) {
    let config = Config::new(HardwareAddress::Ip);

    let mut tracer = Tracer::new(device, |timestamp, packet| {
        if !*TRACE_PACKETS {
            return;
        }
        let text = packet.to_string();
        // Packet's direction field is private; smoltcp prefixes Tx with "-> " and Rx with "<- ".
        let color = if text.starts_with("->") { "31" } else { "32" }; // red out, green in
        println!("\x1b[{color}m[{timestamp}] {}\x1b[0m", text.trim_end());
    });

    let vpn_ip: Ipv4Cidr = first_ipv4(&conf, "Address");

    let gateway_ip = dns_server(&conf);

    let mut iface = Interface::new(config, &mut tracer, smoltcp::time::Instant::now());

    iface.update_ip_addrs(|addrs| {
        addrs
            .push(IpCidr::new(vpn_ip.address().into(), 32))
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

/// Printing every packet is slow, so it's opt-in: set `TRACE_PACKETS`. Always on in tests, where
/// the trace is only shown for failures.
static TRACE_PACKETS: LazyLock<bool> =
    LazyLock::new(|| cfg!(test) || std::env::var_os("TRACE_PACKETS").is_some());

/// The config's DNS server, which is also the VPN gateway.
pub fn dns_server(conf: &Ini) -> Ipv4Address {
    first_ipv4(conf, "DNS")
}

/// The first IPv4 entry of a comma-separated `[Interface]` key; configs often list IPv6 entries too.
fn first_ipv4<T: FromStr>(conf: &Ini, key: &str) -> T {
    conf.get_from(Some("Interface"), key)
        .unwrap_or_else(|| panic!("config is missing [Interface] {key}"))
        .split(',')
        .find_map(|entry| entry.trim().parse().ok())
        .unwrap_or_else(|| panic!("[Interface] {key} has no IPv4 entry"))
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

#[cfg(test)]
mod config_tests {
    use super::*;

    #[test]
    fn first_ipv4_skips_ipv6_entries() {
        let conf = Ini::load_from_str(
            "[Interface]\nAddress = fd00::2/128, 10.2.0.2/32\nDNS = 2606:4700::1111, 10.2.0.1, 1.1.1.1\n",
        )
        .unwrap();
        let addr: Ipv4Cidr = first_ipv4(&conf, "Address");
        assert_eq!(addr.to_string(), "10.2.0.2/32");
        assert_eq!(dns_server(&conf), Ipv4Address::new(10, 2, 0, 1));
    }
}
