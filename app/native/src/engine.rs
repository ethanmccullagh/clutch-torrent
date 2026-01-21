use crate::schema::{ConnectMessage, ProxyMessage, SocketReadChannel, WriteMessage};
use crate::setup::setup_engine;
use bytes::Bytes;
use ini::Ini;
use rand;
use smoltcp::iface::{Context, SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities};
use smoltcp::socket::{icmp, tcp, Socket};
use smoltcp::time::Instant;
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpAddress, TcpPacket, TcpRepr};
use std::collections::HashMap;
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::mpsc;

const TCP_BUFFER_SIZE: usize = 256;

pub fn run_engine(
    conf: Ini,
    device_receiver: mpsc::Receiver<Vec<u8>>,
    vpn_sender: mpsc::Sender<Vec<u8>>,
    local_sender: mpsc::Sender<Vec<u8>>,
    mut control_receiver: mpsc::Receiver<ProxyMessage>,
) {
    let (mut iface, mut device, mut socket_set) =
        setup_engine::<'_>(conf, device_receiver, vpn_sender, local_sender);

    let mut connection_map = HashMap::<SocketHandle, mpsc::Sender<SocketReadChannel>>::new();

    loop {
        let poll_result = iface.poll(Instant::now(), &mut device, &mut socket_set);

        if poll_result {
            poll_sockets(&mut socket_set, &mut connection_map)
        }

        //Poll contrl receiver for Connect/Write/Close messages
        if let Ok(msg) = control_receiver.try_recv() {
            print!("Engine control packet received: ");
            match msg {
                ProxyMessage::Connect { msg } => {
                    println!("Connect");
                    create_socket(msg, &mut socket_set, &mut connection_map, iface.context());
                }
                ProxyMessage::Close { handle, resp } => {
                    let _ = socket_set.remove(handle);
                    let _ = resp.send(true);
                }
                ProxyMessage::Write { msg } => {
                    println!("Write");
                    write_to_socket(msg, &mut socket_set, device.capabilities().clone());
                }
            }
        }

        if let Some(delay) = iface.poll_delay(Instant::now(), &socket_set) {
            // println!("Engine Poll Delay: {:?}", delay.micros());
            // sleep(delay.into());
        } else {
            // sleep(Duration::from_millis(1));
            // println!("No delay");
        }
    }
}

fn poll_sockets(
    socket_set: &mut SocketSet,
    connection_map: &mut HashMap<SocketHandle, mpsc::Sender<SocketReadChannel>>,
) {
    let mut pruned_handles = Vec::<SocketHandle>::new();

    for (handle, socket) in socket_set.iter_mut() {
        let mut rx_buffer = [0u8; TCP_BUFFER_SIZE];
        match socket {
            Socket::Icmp(sock) => {
                if sock.can_recv() {
                    println!("ICMP socket has data");
                    if let Some(sender) = connection_map.get(&handle) {
                        while let Ok((data, addr)) = sock.recv() {
                            match sender.try_send((Bytes::copy_from_slice(data), addr)) {
                                Ok(_) => {}
                                Err(_) => break,
                            }
                        }
                    } else {
                        println!("Sender not found in Connection Map");
                    }
                }
            }
            Socket::Tcp(sock) => {
                println!("testing tcp sock read {}", handle);
                if sock.may_recv() {
                    println!("May recv TCP");
                }
                if sock.can_recv() {
                    println!("TCP socket has data");
                    if let Some(sender) = connection_map.get(&handle) {
                        if sender.is_closed() {
                            println!("TCP Socket {} dead read channel, pruning...", handle);
                            pruned_handles.push(handle);
                            continue;
                        }
                        while sock.can_recv() {
                            if let Ok(len) = sock.peek_slice(&mut rx_buffer) {
                                match sender.try_send((
                                    Bytes::copy_from_slice(&rx_buffer[..len]),
                                    IpAddress::v4(0, 0, 0, 0),
                                )) {
                                    Ok(_) => {}
                                    Err(e) => {
                                        println!("TCP Read Channel Error: {}", e);
                                        break;
                                    }
                                }
                            }
                        }
                    } else {
                        println!("Sender not found in Connection Map");
                    }
                }
            }
            _ => {}
        }
    }

    //Remove Pruned handles from socket set and connection map
    for handle in pruned_handles {
        socket_set.remove(handle);
        connection_map.remove(&handle);

        //test prints
        // println!("Removed pruned handle:");
        // println!("Socket Set len {}", socket_set.iter().count());
        // println!("Connection map len {}", connection_map.len());
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
        ConnectMessage::TCP {
            addr,
            port,
            connect,
            resp,
            read,
        } => {
            let rx_buffer = tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]);
            let tx_buffer = tcp::SocketBuffer::new(vec![0; TCP_BUFFER_SIZE]);
            let mut socket = tcp::Socket::new(rx_buffer, tx_buffer);

            if connect {
                match socket.connect(
                    context,
                    (IpAddress::v4(addr[0], addr[1], addr[2], addr[3]), port),
                    get_ephemeral_port(),
                ) {
                    Ok(()) => {}
                    Err(e) => {
                        println!("{}", e);
                        let _ = resp.send(None);
                        return;
                    }
                }
            } else {
                match socket.listen((IpAddress::v4(10, 2, 0, 2), get_ephemeral_port())) {
                    Ok(()) => {}
                    Err(e) => {
                        println!("{}", e);
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

fn write_to_socket(msg: WriteMessage, socket_set: &mut SocketSet, device_caps: DeviceCapabilities) {
    match msg {
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
        WriteMessage::TCP { handle, data } => {
            let socket = socket_set.get_mut::<tcp::Socket>(handle);

            if !socket.can_send() {
                println!("Tried to write to closed socket");
                return;
            }
            let data_slice = data.iter().as_slice();
            println!("");
            println!("");
            println!("WRITING TCP DATA TO SOCKET: len {}", data_slice.len());
            let _ = socket.send_slice(data_slice);
        }
        _ => {}
    }
}
