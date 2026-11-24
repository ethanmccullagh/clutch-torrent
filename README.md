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

## Desktop app: Clutch

Clutch is a Tauri app in `src-tauri/` with a plain HTML/JS frontend in `ui/` (no npm build step): the base of a torrent client. It has a single torrent list, a settings dialog with a light/dark theme, an add-torrent dialog (magnet link or .torrent file), and a connection dialog that starts and stops the bridge with optional port forwarding; the status bar shows the VPN state, proxy address and announced port. There is no torrent engine yet: added torrents are only listed, as "Waiting for engine".

```sh
cargo tauri dev      # run it (needs tauri-cli: cargo install tauri-cli)
cargo tauri build    # release binary; bundling is off until real icons and metadata exist
```

On Linux, the taskbar icon comes from an installed desktop entry, not from the app itself. For a dev build, install one (and remove it with `--uninstall`):

```sh
scripts/install-desktop-entry.sh          # entry runs target/debug/clutch; pass another path to change
```

The app icons in `src-tauri/icons/` are generated from the logo, `ui/icons/clutch-logo.svg`; after changing it, run `cargo tauri icon ../ui/icons/clutch-logo.svg` from `src-tauri/` (and delete the `android/` and `ios/` folders it also creates).

## Tests

```sh
cargo test
```

Tests read the WireGuard config from `$WG_CONFIG_PATH`, falling back to `cachy3-CA-1208.conf` in the repo root, and need network access. They share one tunnel and run one at a time, because parallel WireGuard sessions with the same key break each other.

## Status

Work in progress. The tunnel works, and TCP, UDP, ICMP and NAT-PMP have been tested through the engine. The SOCKS5 server supports CONNECT and UDP ASSOCIATE (so DHT, uTP and UDP trackers work); and `--forward` accepts incoming connections through a NAT-PMP port mapping.
