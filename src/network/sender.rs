use std::net::UdpSocket;

pub fn send_packet(
    socket: &UdpSocket,
    datagram: &[u8],
    destination: &str,
) -> std::io::Result<()> {

    socket.send_to(datagram, destination)?;

    Ok(())
}