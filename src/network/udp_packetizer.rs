use crate::camera::packetizer_cam::Packet;

/// Header layout (all big-endian), 12 bytes:
///   0..4   frame_id
///   4..8   total_packets
///   8..12  packet_id
pub fn make_udp_datagrams(
    _frame: &[u8],
    packets: &[Packet],
) -> Vec<Vec<u8>> {
    let mut datagrams = Vec::with_capacity(packets.len());

    for packet in packets {
        let mut datagram = Vec::with_capacity(12 + packet.payload.len());

        datagram.extend_from_slice(&packet.frame_id.to_be_bytes());
        datagram.extend_from_slice(&packet.total_packets.to_be_bytes());
        datagram.extend_from_slice(&packet.packet_id.to_be_bytes());
        datagram.extend_from_slice(&packet.payload);

        datagrams.push(datagram);
    }

    datagrams
}
