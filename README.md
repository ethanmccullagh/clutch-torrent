# socks5-vpn-bridge

A userspace WireGuard client that exposes the tunnel as a local SOCKS5 proxy. No root, no TUN device, no kernel module: traffic is encrypted with [boringtun](https://github.com/cloudflare/boringtun) and routed through a [smoltcp](https://github.com/smoltcp-rs/smoltcp) TCP/IP stack running entirely in-process.

## How it works

```
app ──SOCKS5──> socks5.rs ──ProxyMessage──> event_driven_engine (smoltcp) ──IP packets──> boringthreads.rs (boringtun) ──UDP──> WireGuard peer
```

- **`setup.rs`** reads a standard WireGuard `.conf` file and builds the boringtun tunnel and smoltcp interface.
- **`boringthreads.rs`** runs async tasks that encrypt and decrypt packets and exchange them with the peer over UDP.
- **`event_driven_engine/`** is a tokio task that owns the smoltcp stack. It sleeps until a tunnel packet, a `ProxyMessage` (see `schema.rs`), a reader wake-up, or a smoltcp timer arrives, then moves data until nothing more can move. It manages TCP, UDP and ICMP sockets, queues TCP writes until the socket can send, holds received data while a reader's channel is full, and handles NAT-PMP port-mapping requests. A `None` on a socket's read channel means EOF; dropping the read channel closes the socket.
- **`socks5.rs`** is a SOCKS5 server on `127.0.0.1:1080` (CONNECT and UDP ASSOCIATE, IPv4 only, no auth) that turns client connections into engine sockets. Hostnames are resolved through the tunnel, so DNS doesn't leak.

## Usage

```sh
cargo run -- path/to/wireguard.conf   # defaults to cachy3-CA-1208.conf
```

Then point your client at the SOCKS5 proxy `127.0.0.1:1080`.

To accept incoming connections (seeding), pass the client's listen port. The bridge maps a port on the VPN server with NAT-PMP, keeps the mapping alive, forwards incoming TCP to `127.0.0.1:<port>`, and delivers incoming UDP through the client's SOCKS5 UDP association. Set the client's announced port to the public port it prints:

```sh
cargo run -- path/to/wireguard.conf --forward 6881
``` Set `TRACE_PACKETS=1` to print every packet.

## Tests

```sh
cargo test
```

Tests read the WireGuard config from `$WG_CONFIG_PATH`, falling back to `cachy3-CA-1208.conf` in the repo root, and need network access. They share one tunnel and run one at a time, because parallel WireGuard sessions with the same key break each other.

## Status

Work in progress. The tunnel works, and TCP, UDP, ICMP and NAT-PMP have been tested through the engine. The SOCKS5 server supports CONNECT and UDP ASSOCIATE (so DHT, uTP and UDP trackers work); and `--forward` accepts incoming connections through a NAT-PMP port mapping.
