use crate::schema::{ChannelMessage, ProxyMessage, SocketType};
use crate::setup::setup_engine;
use anyhow::Result;
use boringtun::noise::TunnResult;
use byteorder::{ByteOrder, NetworkEndian};
use ini::Ini;
use smoltcp::iface::{Interface, SocketHandle, SocketSet};
use smoltcp::phy::{wait, Device, DeviceCapabilities};
use smoltcp::socket::{icmp, tcp, udp, Socket};
use smoltcp::time::Instant;
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpAddress, IpListenEndpoint};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::os::unix::io::AsRawFd;
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::sync::{mpsc, oneshot};

// --- CONFIGURATION (Replace these with your ProtonVPN details) ---

pub fn run_engine(
    conf: Ini,
    device_receiver: mpsc::Receiver<Vec<u8>>,
    vpn_sender: mpsc::Sender<Vec<u8>>,
    local_sender: mpsc::Sender<Vec<u8>>,
    mut control_receiver: mpsc::Receiver<ProxyMessage>,
) {
    let (mut iface, mut device, mut socket_set) =
        setup_engine::<'_>(conf, device_receiver, vpn_sender, local_sender);

    loop {
        iface.poll(Instant::now(), &mut device, &mut socket_set);
        control_recv(
            &mut socket_set,
            device.capabilities().clone(),
            &mut control_receiver,
        );
    }
}

pub fn control_recv(
    socket_set: &mut SocketSet,
    device_caps: DeviceCapabilities,
    control_receiver: &mut mpsc::Receiver<ProxyMessage>,
) {
    if let Ok(msg) = control_receiver.try_recv() {
        print!("Engine control packet received: ");
        match msg {
            ProxyMessage::Connect {
                addr,
                port,
                resp,
                socket_type,
            } => {
                println!("Connect");
                create_socket(addr, port, resp, socket_type, socket_set);
            }
            ProxyMessage::Close { handle, resp } => {
                println!("Close");
                let _ = socket_set.remove(handle);
                let _ = resp.send(true);
            }
            ProxyMessage::Read { handle, resp } => {
                println!("Read");
                read_from_socket(handle, resp, socket_set);
            }
            ProxyMessage::Write {
                handle,
                data,
                dst,
                resp,
                socket_type,
                send_at,
            } => {
                println!("Write");
                write_to_socket(
                    handle,
                    data,
                    dst,
                    resp,
                    socket_type,
                    socket_set,
                    device_caps,
                );
            }
        }
    }
}

fn create_socket(
    addr: Option<[u8; 4]>,
    port: Option<u16>,
    resp: oneshot::Sender<Option<SocketHandle>>,
    socket_type: SocketType,
    socket_set: &mut SocketSet,
) {
    match socket_type {
        SocketType::TCP => {
            let rx_buffer = tcp::SocketBuffer::new(vec![0; 16384]);
            let tx_buffer = tcp::SocketBuffer::new(vec![0; 16384]);
            let socket = tcp::Socket::new(rx_buffer, tx_buffer);

            let handle = socket_set.add(socket);

            resp.send(Some(handle));
        }
        SocketType::ICMP => {
            let rx_buffer =
                icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]);
            let tx_buffer =
                icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]);
            let socket = icmp::Socket::new(rx_buffer, tx_buffer);

            let handle = socket_set.add(socket);

            resp.send(Some(handle));
        }
        _ => {}
    }
}

fn read_from_socket(
    handle: SocketHandle,
    resp: oneshot::Sender<Option<Vec<u8>>>,
    socket_set: &mut SocketSet,
) {
    let socket = socket_set.get_mut::<tcp::Socket>(handle);
    if socket.can_recv() {
        let mut buffer = vec![0u8; 4096];
        match socket.recv_slice(&mut buffer) {
            Ok(len) if len > 0 => {
                buffer.truncate(len);
                println!("ENGINE: Read {} bytes from socket", len);
                let _ = resp.send(Some(buffer));
                return;
            }
            _ => {}
        }
    }
    let _ = resp.send(None);
}

fn write_to_socket(
    handle: SocketHandle,
    data: Vec<u8>,
    dst: Option<[u8; 4]>,
    resp: oneshot::Sender<bool>,
    socket_type: SocketType,
    socket_set: &mut SocketSet,
    device_caps: DeviceCapabilities,
) {
    let timestamp = Instant::now();
    match socket_type {
        SocketType::ICMP => {
            let socket = socket_set.get_mut::<icmp::Socket>(handle);

            let remote_addr = IpAddress::v4(8, 8, 8, 8);

            if !socket.is_open() {
                socket.bind(icmp::Endpoint::Ident(0x1234)).unwrap();
            }

            let mut echo_payload = [0xffu8; 40];

            let send_at = timestamp.total_millis()+200;

            NetworkEndian::write_i64(&mut echo_payload, send_at);

            let repr = Icmpv4Repr::EchoRequest {
                ident: 0x1234,
                seq_no: 1,
                data: &echo_payload,
            };

            let icmp_payload = socket.send(repr.buffer_len(), remote_addr).unwrap();
            let mut icmp_packet = Icmpv4Packet::new_unchecked(icmp_payload);

            repr.emit(&mut icmp_packet, &device_caps.checksum);

            let _ = resp.send(true);
        }
        SocketType::TCP => {}
        SocketType::UDP => {}
    }
}
