use crate::schema::{ConnectMessage, ProxyMessage, SocketReadChannel, WriteMessage};
use crate::setup::setup_engine;
use bytes::Bytes;
use ini::Ini;
use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::phy::{Device, DeviceCapabilities};
use smoltcp::socket::{icmp, tcp, Socket};
use smoltcp::time::Instant;
use smoltcp::wire::{Icmpv4Packet, Icmpv4Repr, IpAddress};
use std::collections::HashMap;
use std::thread::sleep;
use std::time::Duration;
use tokio::sync::mpsc;

pub async fn run_engine(
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
        iface.poll(Instant::now(), &mut device, &mut socket_set);

        control_recv(
            &mut socket_set,
            device.capabilities().clone(),
            &mut control_receiver,
            &mut connection_map,
        );
        poll_sockets(&mut socket_set, &mut connection_map);


        loop{
            match control_receiver.poll_recv() {
                Poll::Ready
            }

            match device_receiver.poll_recv(
        }


        if let Some(delay) = iface.poll_delay(Instant::now(), &socket_set) {
            println!("Engine Poll Delay: {:?}", delay.micros());
            sleep(delay.into());
        } else {
            sleep(Duration::from_millis(1));
            // println!("No delay");
        }
    }
}

fn poll_sockets(
    socket_set: &mut SocketSet,
    connection_map: &mut HashMap<SocketHandle, mpsc::Sender<SocketReadChannel>>,
) {
    for (handle, socket) in socket_set.iter_mut() {
        let mut rx_buffer = [0u8; 16384];
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
                if sock.can_recv() {
                    if let Some(sender) = connection_map.get(&handle) {
                        while sock.can_recv() {
                            if let Ok(len) = sock.peek_slice(&mut rx_buffer) {
                                match sender.try_send((
                                    Bytes::copy_from_slice(&rx_buffer[..len]),
                                    IpAddress::v4(0, 0, 0, 0),
                                )) {
                                    Ok(_) => {}
                                    Err(_) => {
                                        println!("TCP READ CHANNEL FULL");
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
}

fn control_recv(
    socket_set: &mut SocketSet,
    device_caps: DeviceCapabilities,
    control_receiver: &mut mpsc::Receiver<ProxyMessage>,
    connection_map: &mut HashMap<SocketHandle, mpsc::Sender<SocketReadChannel>>,
) {
    if let Ok(msg) = control_receiver.try_recv() {
        print!("Engine control packet received: ");
        match msg {
            ProxyMessage::Connect { msg } => {
                create_socket(msg, socket_set, connection_map);
            }
            ProxyMessage::Close { handle, resp } => {
                let _ = socket_set.remove(handle);
                let _ = resp.send(true);
            }
            ProxyMessage::Write { msg } => {
                write_to_socket(msg, socket_set, device_caps);
            }
        }
    } else {
        // println!("No Control Message in channel");
    }
}

fn create_socket(
    msg: ConnectMessage,
    socket_set: &mut SocketSet,
    connection_map: &mut HashMap<SocketHandle, mpsc::Sender<SocketReadChannel>>,
) {
    match msg {
        ConnectMessage::TCP {
            addr,
            port,
            connect,
            resp,
        } => {
            let rx_buffer = tcp::SocketBuffer::new(vec![0; 16384]);
            let tx_buffer = tcp::SocketBuffer::new(vec![0; 16384]);
            let socket = tcp::Socket::new(rx_buffer, tx_buffer);

            let handle = socket_set.add(socket);

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
        _ => {}
    }
}
