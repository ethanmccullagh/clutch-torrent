use crate::schema::{ConnectMessage, ProxyMessage, SocketReadChannel, WriteMessage};
use crate::setup::setup_engine;
use bytes::Bytes;
use ini::Ini;
use rand;
use smoltcp::iface::{Context, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities};
use smoltcp::socket::{icmp, tcp, udp, Socket};
use smoltcp::time::Instant;
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpAddress, IpEndpoint, IpListenEndpoint};
use std::collections::HashMap;
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::mpsc;

const TCP_BUFFER_SIZE: usize = 256;
const UDP_BUFFER_SIZE: usize = 256;

pub fn run_engine(
    conf: Ini,
    device_receiver: mpsc::Receiver<Vec<u8>>,
    vpn_sender: mpsc::Sender<Vec<u8>>,
    mut control_receiver: mpsc::Receiver<ProxyMessage>,
) {
    let (mut iface, mut device, mut socket_set) =
        setup_engine::<'_>(conf, device_receiver, vpn_sender);

    let mut connection_map = HashMap::<SocketHandle, mpsc::Sender<SocketReadChannel>>::new();
    let mut pruned_sockets = Vec::<SocketHandle>::new();
    let mut cycles = 0;
    let mut dead_cycles = 0;

    loop {
        //Remove Pruned handles from socket set and connection map
        pruned_sockets.iter().for_each(|handle| {
            socket_set.remove(*handle);
            connection_map.remove(handle);
        });
        pruned_sockets.clear();

        //Poll Sockets
        if iface.poll(Instant::now(), &mut device, &mut socket_set) {
            poll_sockets(&mut socket_set, &mut connection_map, &mut pruned_sockets);
            dead_cycles = 0;
        }

        //Poll contrl receiver for Connect/Write/Close messages
        if let Ok(msg) = control_receiver.try_recv() {
            eprint!("Engine control message received: ");
            match msg {
                ProxyMessage::Connect { msg } => {
                    eprintln!("Connect");
                    create_socket(msg, &mut socket_set, &mut connection_map, iface.context());
                }
                ProxyMessage::Write { msg } => {
                    eprintln!("Write");
                    write_to_socket(
                        msg,
                        &mut socket_set,
                        device.capabilities().clone(),
                        &mut connection_map,
                    );
                }
                ProxyMessage::Close => {
                    eprintln!("Engine Shutdown -- Terminating process");
                    break;
                }
            }
        }

        cycles += 1;
        dead_cycles += 1;
        //Sleep if there is a poll delay
        if let Some(delay) = iface.poll_delay(Instant::now(), &socket_set) {
            eprintln!("Engine Poll Delay: {:?}", delay.micros());
            eprintln!("Engine Cycles: {:?}", cycles);

            sleep(delay.into());
        } else if dead_cycles > 10 {
            sleep(Duration::from_millis(100));
        }
        if cycles % 1000 == 0 {
            eprintln!("Engine Cycles: {:?}", cycles);
        }
    }
}

fn poll_sockets(
    socket_set: &mut SocketSet,
    connection_map: &mut HashMap<SocketHandle, mpsc::Sender<SocketReadChannel>>,
    pruned_sockets: &mut Vec<SocketHandle>,
) {
    for (handle, socket) in socket_set.iter_mut() {
        let mut rx_buffer = [0u8; TCP_BUFFER_SIZE];
        match socket {
            Socket::Icmp(sock) => {
                if let Some(sender) = connection_map.get(&handle) {
                    if sender.is_closed() {
                        eprintln!("ICMP Socket {} dead read channel, pruning...", handle);
                        pruned_sockets.push(handle);
                        continue;
                    }

                    while let Ok((data, addr)) = sock.recv() {
                        match sender.try_send(Some((Bytes::copy_from_slice(data), addr))) {
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                }
            }
            Socket::Tcp(sock) => {
                if let Some(sender) = connection_map.get(&handle) {
                    if sender.is_closed() {
                        eprintln!("TCP Socket {} dead read channel, pruning...", handle);
                        pruned_sockets.push(handle);
                        sock.close();
                        continue;
                    }

                    while sock.can_recv() {
                        if let Ok(len) = sock.recv_slice(&mut rx_buffer) {
                            match sender.try_send(Some((
                                Bytes::copy_from_slice(&rx_buffer[..len]),
                                IpAddress::v4(0, 0, 0, 0),
                            ))) {
                                Ok(_) => {}
                                Err(e) => {
                                    eprintln!("TCP Read Channel Error: {}", e);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            Socket::Udp(sock) => {
                if let Some(sender) = connection_map.get(&handle) {
                    if sender.is_closed() {
                        eprintln!("UDP Socket {} dead read channel, pruning...", handle);
                        pruned_sockets.push(handle);
                        sock.close();
                        continue;
                    }

                    while sock.can_recv() {
                        if let Ok((len, meta_data)) = sock.recv_slice(&mut rx_buffer) {
                            match sender.try_send(Some((
                                Bytes::copy_from_slice(&rx_buffer[..len]),
                                meta_data.endpoint.addr,
                            ))) {
                                Ok(_) => {}
                                Err(e) => {
                                    eprintln!("TCP Read Channel Error: {}", e);
                                    break;
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn get_ephemeral_port() -> u16 {
    49152 + (rand::random::<u16>() % 16384)
}

fn create_socket(
    msg: ConnectMessage,
    socket_set: &mut SocketSet,
    connection_map: &mut HashMap<SocketHandle, mpsc::Sender<SocketReadChannel>>,
    context: &mut Context,
) {
    match msg {
        ConnectMessage::UDP {
            local_port,
            resp,
            read,
        } => {
            let rx_buffer =
                udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY], vec![0; UDP_BUFFER_SIZE]);
            let tx_buffer =
                udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY], vec![0; UDP_BUFFER_SIZE]);
            let mut socket = udp::Socket::new(rx_buffer, tx_buffer);

            let mut port = local_port;
            if port == 0 {
                port = get_ephemeral_port();
            }

            match socket.bind(IpListenEndpoint {
                addr: Some(IpAddress::v4(10, 2, 0, 2)),
                port: port,
            }) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("smoltcp UDP bind error: {}", e);
                    let _ = resp.send(None);
                    return;
                }
            }

            let handle = socket_set.add(socket);
            connection_map.insert(handle, read);

            let _ = resp.send(Some(handle));
        }
        ConnectMessage::TCP {
            remote,
            local_port,
            connect,
            resp,
            read,
        } => {
            let rx_buffer = tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]);
            let tx_buffer = tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]);
            let mut socket = tcp::Socket::new(rx_buffer, tx_buffer);

            let mut port = local_port;
            if port == 0 {
                port = get_ephemeral_port();
            }

            if connect {
                match socket.connect(context, remote, port) {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("smoltcp TCP connect error: {}", e);
                        let _ = resp.send(None);
                        return;
                    }
                }
            } else {
                match socket.listen(port) {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("smoltcp TCP listen error: {}", e);
                        let _ = resp.send(None);
                        return;
                    }
                }
            }

            let handle = socket_set.add(socket);
            connection_map.insert(handle, read);

            let _ = resp.send(Some(handle));
        }
        ConnectMessage::ICMP { resp, read } => {
            let rx_buffer =
                icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]);
            let tx_buffer =
                icmp::PacketBuffer::new(vec![icmp::PacketMetadata::EMPTY], vec![0; 256]);
            let socket = icmp::Socket::new(rx_buffer, tx_buffer);

            let handle = socket_set.add(socket);

            connection_map.insert(handle, read);

            let _ = resp.send(Some(handle));
        }
    }
}

fn write_to_socket(
    msg: WriteMessage,
    socket_set: &mut SocketSet,
    device_caps: DeviceCapabilities,
    connection_map: &mut HashMap<SocketHandle, mpsc::Sender<SocketReadChannel>>,
) {
    match msg {
        WriteMessage::UDP {
            handle,
            data,
            dst,
            dst_port,
        } => {
            let socket = socket_set.get_mut::<udp::Socket>(handle);
            let remote_endpoint =
                IpEndpoint::new(IpAddress::v4(dst[0], dst[1], dst[2], dst[3]), dst_port);

            match socket.send_slice(&data, remote_endpoint) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("UDP socket {} send error", handle);
                    eprintln!("{}", e)
                }
            }
        }
        WriteMessage::TCP { handle, data } => {
            let socket = socket_set.get_mut::<tcp::Socket>(handle);

            if !socket.can_send() {
                let sender = connection_map.get_mut(&handle).unwrap();
                sender.send(None);
                eprintln!("Tried to write to closed socket");
                return;
            }
            let data_slice = data.iter().as_slice();
            let _ = socket.send_slice(data_slice);
        }
        WriteMessage::ICMP {
            handle,
            dst,
            ident,
            seq,
        } => {
            let socket = socket_set.get_mut::<icmp::Socket>(handle);

            let remote_addr = IpAddress::v4(dst[0], dst[1], dst[2], dst[3]);

            if !socket.is_open() {
                socket.bind(icmp::Endpoint::Ident(0x1234)).unwrap();
            }
            let repr = Icmpv4Repr::EchoRequest {
                ident: ident,
                seq_no: seq,
                data: &[],
            };

            let icmp_payload = socket.send(repr.buffer_len(), remote_addr).unwrap();
            let mut icmp_packet = Icmpv4Packet::new_unchecked(icmp_payload);

            repr.emit(&mut icmp_packet, &device_caps.checksum);
        }
        WriteMessage::NAT { handle, opcode } => {
            let socket = socket_set.get_mut::<udp::Socket>(handle);
            let remote_endpoint = IpEndpoint::new(IpAddress::v4(10, 2, 0, 1), 5351);
            let port: [u8; 2] = socket.endpoint().port.to_be_bytes();

            let data = [
                //V  Op      Reserved  IntPort           ExtPort   Lifetime
                0x0, opcode, 0x0, 0x0, port[0], port[1], 0x0, 0x0, 0x0, 0x0, 0x0E, 0x10,
            ];

            match socket.send_slice(&data, remote_endpoint) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("UDP socket {} send error", handle);
                    eprintln!("{}", e)
                }
            }
        }
    }
}
