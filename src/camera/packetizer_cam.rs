pub struct Packet {
    pub frame_id: u32,
    pub packet_id: u32,
    pub total_packets: u32,
    pub payload: Vec<u8>,
}


const MAX_PAYLOAD: usize = 1200;

pub fn packetize(
    frame_id: u32,
    frame: &[u8],
) -> Vec<Packet> {

    let total_packets =
        (frame.len() + MAX_PAYLOAD - 1) / MAX_PAYLOAD;  //total packet making dividing by frame byte/1200

    let mut packets = Vec::new();     

    for (packet_id, chunk) in frame
        .chunks(MAX_PAYLOAD)
        .enumerate()
    {
        let packet = Packet {
            frame_id,
            packet_id: packet_id as u32,
            total_packets: total_packets as u32,
            payload: chunk.to_vec(),
        };

        packets.push(packet);
    }

    packets
}