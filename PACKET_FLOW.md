# Packet flow

How traffic from a SOCKS5 client (here a torrent client, treated as a black box) gets through
the WireGuard tunnel and back. Everything runs in one process with no TUN device and no root;
the TCP/IP stack is smoltcp, running in user space.

## Overview

```
 ┌──────────────────────────────┐
 │ Torrent client (black box)   │
 └──────┬────────────────┬──────┘
        │ TCP            │ UDP datagrams (SOCKS5 UDP header)
        │ 127.0.0.1:1080 │ 127.0.0.1:<relay port from UDP ASSOCIATE reply>
        ▼                ▼
 ┌─────────────────────────────────────────────────────────────────────┐
 │ SOCKS5 server (socks5.rs), one task per client connection          │
 │   CONNECT        → relay(): client TcpStream ⇄ engine TCP socket    │
 │   UDP ASSOCIATE  → relay UdpSocket ⇄ engine UDP socket              │
 │   hostnames      → ProxyMessage::Resolve (never the host resolver)  │
 └──────┬───────────────────────────────────────────────▲─────────────┘
        │ control channel (mpsc, 100):                  │ per-socket read channel (mpsc, 100):
        │ ProxyMessage::Connect / Write / Resolve       │ Some((data, from)) | None = EOF
        ▼                                               │
 ┌─────────────────────────────────────────────────────────────────────┐
 │ Engine task (event_driven_engine/): smoltcp Interface + SocketSet   │
 │   TCP, UDP, ICMP and DNS sockets; IPv4 only; MTU 1420               │
 │   address and DNS server from the WireGuard config                  │
 │   wakes on: tunnel packet, control message, reader wake-up, timer   │
 └──────┬───────────────────────────────────────────────▲─────────────┘
        │ raw IPv4 packets (QueueDevice → vpn_sender,   │ raw IPv4 packets
        │ unbounded)                                    │ (device_receiver, mpsc 100)
        ▼                                               │
 ┌─────────────────────────────────────────────────────────────────────┐
 │ WireGuard tasks (boringthreads.rs), one boringtun Tunn             │
 │   transmit: encapsulate → send_to(endpoint)                         │
 │   receive:  recv_from (endpoint only) → decapsulate → engine        │
 │             (IPv6 packets from the tunnel are dropped)              │
 │   timers:   every 250 ms: handshakes, keepalives, rekeys            │
 └──────┬───────────────────────────────────────────────▲─────────────┘
        │ one UDP socket 0.0.0.0:<ephemeral>,           │
        │ encrypted, to the config's Endpoint           │
        ▼                                               │
 ┌─────────────────────────────────────────────────────────────────────┐
 │ Proton WireGuard server → internet (source = the VPN's public IP)   │
 └─────────────────────────────────────────────────────────────────────┘
```

The only socket that touches the real network is the WireGuard UDP socket. Everything the
client sends leaves as encrypted UDP to the VPN server.

## Outgoing TCP (SOCKS5 CONNECT)

```
client          SOCKS5 task                 engine                    tunnel → server → peer
  │ greeting (no auth) │                       │                              │
  │ CONNECT host:port  │                       │                              │
  │───────────────────▶│ (hostname?) Resolve ─▶│ DNS query (A) ──────────────▶│
  │                    │◀──────────── IPv4 ────│◀──────────── answer ─────────│
  │                    │ Connect TCP ─────────▶│ SYN ────────────────────────▶│
  │                    │                       │◀─────────────────── SYN/ACK ─│
  │                    │◀──── handle ──────────│ (reply only after handshake; │
  │◀── SUCCEEDED ──────│                       │  None on reset / 60 s)       │
  │ data ─────────────▶│ Write TCP ───────────▶│ segments ───────────────────▶│
  │◀────────────── data│◀── Some((data,from)) ─│◀─────────────────── segments │
  │ close ────────────▶│ drop read channel ───▶│ FIN / close ────────────────▶│
```

- The client's half-close is not passed on: the Proton server answers a FIN with a RST.
- The Proton server completes every handshake itself, so a dead destination still gets
  SUCCEEDED, followed by silence.
- IPv6 destinations get ADDRESS_TYPE_NOT_SUPPORTED; unresolvable names get HOST_UNREACHABLE.

## Outgoing UDP (SOCKS5 UDP ASSOCIATE)

```
client                         SOCKS5 task                          engine → tunnel
  │ UDP ASSOCIATE (TCP) ───────▶│ bind relay 127.0.0.1:<ephemeral>   │
  │                             │ Connect UDP ──────────────────────▶│ new UDP socket
  │◀── SUCCEEDED + relay addr ──│                                    │  (32 packets each way)
  │ datagram + SOCKS5 header ──▶│ strip header, resolve name ───────▶│ Write UDP → packet
  │   (to relay port)           │   (cached per association)         │
  │◀── datagram + header ───────│◀──────── Some((data, from)) ───────│◀── reply packet
  │ close the TCP connection ──▶│ association ends, sockets dropped  │
```

- The first sender on the relay is taken as the client; datagrams from anyone else are ignored.
- Fragmented datagrams and IPv6 destinations are dropped.

## DNS

Hostnames from CONNECT requests and UDP datagrams are resolved by the engine's smoltcp DNS
socket (A records only) against the DNS server in the WireGuard config, so lookups go through
the tunnel like any other packet. The host's resolver is never used.

## Incoming (with port forwarding, `--forward <port>`)

```
peer → VPN public IP:<mapped port> → Proton server → tunnel → engine
   TCP: one of 8 engine listening sockets accepts → TcpStream to 127.0.0.1:<port> → relay()
   UDP: the engine UDP socket bound to <port> → the first UDP ASSOCIATE's relay → client
```

- The mapping is made with NAT-PMP to the gateway (10.2.0.1:5351), from an engine UDP socket
  inside the tunnel, and renewed at half the granted lifetime.
- Incoming connections appear to come from the gateway, not the real peer (Proton rewrites
  the source address).
- For incoming TCP, the client (black box) must listen on 127.0.0.1:<port>.

## While the VPN is disconnected (Clutch)

```
client → 127.0.0.1:<proxy port> → refuse_connections: accept, then close at once
```

Clutch holds the proxy port from startup until exit. When the bridge stops, the refuser takes
over the same listener, so the client's connections fail immediately and no other program can
bind the port. Nothing reaches the network until the bridge starts again.
