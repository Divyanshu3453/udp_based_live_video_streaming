mod camera;
mod network;

use std::net::UdpSocket;
use std::time::{Duration, Instant};

// Gap between consecutive datagrams. Sending a whole frame (60-170 packets)
// in one instant burst overflows the receiver's socket buffer; a tiny gap
// spreads the frame over a few milliseconds and removes that loss.
const PACING: Duration = Duration::from_micros(30);

fn main() -> std::io::Result<()> {
    // UDP socket for sending
    let socket = UdpSocket::bind("0.0.0.0:0")?;

    // Temporary receiver address
    let destination = "127.0.0.1:8000";

    let mut frames_sent: u32 = 0;
    let mut send_errors: u32 = 0;

    camera::reciever::receive_frames(|frame, packets| {
        // Frame + camera packet details -> UDP datagrams
        let datagrams = network::udp_packetizer::make_udp_datagrams(&frame, &packets);

        let mut next_slot = Instant::now();

        for datagram in &datagrams {
            // wait for our turn (spin + yield: sleep() is far too coarse,
            // especially on Windows)
            while Instant::now() < next_slot {
                std::thread::yield_now();
            }

            if let Err(e) = network::sender::send_packet(&socket, datagram, destination) {
                send_errors += 1;
                if send_errors <= 5 {
                    println!("UDP send failed: {e}");
                }
            }

            next_slot = Instant::now() + PACING;
        }

        frames_sent += 1;
        if frames_sent % 30 == 0 {
            println!(
                "UDP: {} frames sent ({} datagrams in the last frame, {} send errors)",
                frames_sent,
                datagrams.len(),
                send_errors
            );
        }
    });

    Ok(())
}
