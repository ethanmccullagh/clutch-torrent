# socks5-vpn-bridge

A userspace WireGuard client that exposes the tunnel as a local SOCKS5 proxy. No root, no TUN device, no kernel module: traffic is encrypted with [boringtun](https://github.com/cloudflare/boringtun) and routed through a [smoltcp](https://github.com/smoltcp-rs/smoltcp) TCP/IP stack running entirely in-process.

## How it works

```
app ──SOCKS5──> socks5.rs ──ProxyMessage──> engine.rs (smoltcp) ──IP packets──> boringthreads.rs (boringtun) ──UDP──> WireGuard peer
```

- **`setup.rs`** reads a standard WireGuard `.conf` file and builds the boringtun tunnel and smoltcp interface.
- **`boringthreads.rs`** runs async tasks that encrypt and decrypt packets and exchange them with the peer over UDP.
- **`engine.rs`** is a synchronous loop that drives the smoltcp interface. It creates and polls TCP, UDP and ICMP sockets in response to `ProxyMessage`s (see `schema.rs`) and handles NAT-PMP port-mapping requests.
- **`socks5.rs`** is a SOCKS5 server on `127.0.0.1:1080` (CONNECT only, no auth) that turns client connections into engine sockets.

## Usage

The config path is currently hardcoded in `src/main.rs`. Point it at your WireGuard config, then:

```sh
cargo run
```

## Tests

```sh
cargo test
```

Tests read the WireGuard config from `$WG_CONFIG_PATH`, falling back to `cachy3-CA-1208.conf` in the repo root, and need network access. Tests that open a tunnel take turns through a shared lock, because parallel WireGuard sessions with the same key break each other.

## Status

Work in progress. The tunnel works, and TCP, UDP, ICMP and NAT-PMP have been tested through the engine. The SOCKS5 server is written but `main.rs` does not start it yet.
