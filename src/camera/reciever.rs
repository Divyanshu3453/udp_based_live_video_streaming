
use std::io::Read;
use std::net::TcpListener;

use super::packetizer_cam;

pub fn receive_frames<F>(mut process_frame: F)
where
    F: FnMut(Vec<u8>, Vec<packetizer_cam::Packet>),
{
    let listener = TcpListener::bind("127.0.0.1:9000")
        .expect("Failed to bind TCP socket");

    println!("Rust waiting for Python...");

    let (mut stream, address) = listener
        .accept()
        .expect("Failed to accept connection");

    println!("Python connected from {}", address);
    let _ = stream.set_nodelay(true);

    let mut frame_id: u32 = 0;

    loop {
        // Receive the 4-byte frame size
        let mut length_buffer = [0u8; 4];

        match stream.read_exact(&mut length_buffer) {
            Ok(_) => {}

            Err(_) => {
                println!("Python disconnected.");
                break;
            }
        }

        let frame_size = u32::from_be_bytes(length_buffer);

        // sanity check: refuse nonsense sizes instead of allocating GBs
        if frame_size == 0 || frame_size > 32 * 1024 * 1024 {
            println!("Bad frame size {}, closing.", frame_size);
            break;
        }

        // Receive complete JPEG
        let mut frame = vec![0u8; frame_size as usize];

        match stream.read_exact(&mut frame) {
            Ok(_) => {}

            Err(_) => {
                println!("Failed to receive complete frame.");
                break;
            }
        }

        // Camera-level packetization
        let packets =
            packetizer_cam::packetize(frame_id, &frame);

        if frame_id % 30 == 0 {
            println!(
                "Frame {}: {} bytes -> {} packets",
                frame_id,
                frame.len(),
                packets.len()
            );
        }

        

        // Give BOTH frame and camera packets
        process_frame(frame, packets);

        frame_id += 1;
    }
}