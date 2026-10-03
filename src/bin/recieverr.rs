// ======================================================================
// UDP receiver  ->  frame reassembly  ->  TCP to Python
//
// Two threads:
//   * main thread   : ONLY reads UDP and reassembles frames (never blocks)
//   * python thread : ONLY writes finished frames to Python over TCP
//
// Why: in the old version the UDP loop also did the TCP write to Python.
// Whenever Python was slow (imshow, decode, ...) the UDP loop stalled, the
// OS UDP buffer overflowed, and packets were silently dropped.
// ======================================================================

use std::collections::HashMap;
use std::io::{ErrorKind, Write};
use std::net::{TcpListener, UdpSocket};
use std::sync::mpsc::{sync_channel, Receiver, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

// ---------------------------- settings --------------------------------

const UDP_ADDR: &str = "0.0.0.0:8000";
const PYTHON_ADDR: &str = "127.0.0.1:9001";

const HEADER_LEN: usize = 12; // frame_id, total_packets, packet_id (u32 BE)
const MAX_PAYLOAD: usize = 1200; // must match camera/packetizer_cam.rs
const MAX_PACKETS_PER_FRAME: u32 = 20_000; // sanity limit (~24 MB frame)

const RECV_BUFFER_BYTES: usize = 8 * 1024 * 1024; // ask OS for 8 MB
const FRAME_TIMEOUT: Duration = Duration::from_millis(250); // give up on a frame
const IDLE_RESET: Duration = Duration::from_secs(2); // sender restarted?
const MAX_IN_FLIGHT: usize = 8;

// false -> incomplete frames are dropped (clean video, recommended)
// true  -> incomplete frames are forwarded with missing packets
//          ZERO-FILLED. For experiments only: JPEG entropy-coded data
//          desynchronises after a hole, so the picture after the hole is
//          usually garbage (sometimes decode fails). It is NOT recovery.
const FORWARD_INCOMPLETE: bool = false;

// ----------------------------- helpers --------------------------------

/// frame_a is newer than frame_b (wrap-around safe)
fn is_newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000_0000
}

/// A JPEG must start with FFD8 and end with FFD9
fn looks_like_jpeg(data: &[u8]) -> bool {
    data.len() > 4
        && data[0] == 0xFF
        && data[1] == 0xD8
        && data[data.len() - 2] == 0xFF
        && data[data.len() - 1] == 0xD9
}

struct FrameAssembly {
    total: u32,
    slots: Vec<Option<Vec<u8>>>,
    received: u32,
    first_seen: Instant,
}

impl FrameAssembly {
    fn new(total: u32) -> Self {
        Self {
            total,
            slots: vec![None; total as usize],
            received: 0,
            first_seen: Instant::now(),
        }
    }

    fn is_complete(&self) -> bool {
        self.received == self.total
    }

    /// Exact byte-for-byte reconstruction (only valid when complete)
    fn build_complete(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.total as usize * MAX_PAYLOAD);
        for slot in &self.slots {
            out.extend_from_slice(slot.as_ref().unwrap());
        }
        out
    }

    /// Reconstruction with holes filled by zeros (experiments only)
    fn build_zero_filled(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.total as usize * MAX_PAYLOAD);
        for (i, slot) in self.slots.iter().enumerate() {
            match slot {
                Some(p) => out.extend_from_slice(p),
                None if i + 1 < self.slots.len() => {
                    out.extend(std::iter::repeat(0u8).take(MAX_PAYLOAD))
                }
                None => {} // last packet missing: length unknown, skip
            }
        }
        out
    }
}

#[derive(Default)]
struct Stats {
    complete: u32,
    lost: u32,         // frames abandoned with missing packets
    bad_jpeg: u32,     // complete but not a valid JPEG (should stay 0!)
    dropped_slow: u32, // Python too slow, frame discarded
    stale: u32,        // packets of frames already finished
    bad_packets: u32,  // malformed / inconsistent datagrams
    datagrams: u32,
}

// ------------------------ TCP -> Python thread ------------------------

fn python_thread(rx: Receiver<Vec<u8>>) {
    let listener = TcpListener::bind(PYTHON_ADDR).expect("bind TCP 9001");
    println!("Waiting for Python display on {PYTHON_ADDR} ...");

    loop {
        let (mut stream, addr) = match listener.accept() {
            Ok(v) => v,
            Err(e) => {
                println!("accept failed: {e}");
                continue;
            }
        };
        println!("Python display connected from {addr}");
        let _ = stream.set_nodelay(true);

        // throw away frames that piled up while nobody was connected
        while rx.try_recv().is_ok() {}

        for frame in rx.iter() {
            // header + body in ONE write (no tiny extra TCP segment)
            let mut out = Vec::with_capacity(4 + frame.len());
            out.extend_from_slice(&(frame.len() as u32).to_be_bytes());
            out.extend_from_slice(&frame);

            if let Err(e) = stream.write_all(&out) {
                println!("Python disconnected: {e}");
                break; // go back to accept()
            }
        }
    }
}

// ------------------------------- main ---------------------------------

fn main() -> std::io::Result<()> {
    // ---- UDP socket with a BIG receive buffer, bound BEFORE anything else
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = socket.set_recv_buffer_size(RECV_BUFFER_BYTES);
    socket.bind(&UDP_ADDR.parse::<std::net::SocketAddr>().unwrap().into())?;
    let socket: UdpSocket = socket.into();
    socket.set_read_timeout(Some(Duration::from_millis(20)))?;

    let granted = socket2::SockRef::from(&socket)
        .recv_buffer_size()
        .unwrap_or(0);
    println!("UDP listening on {UDP_ADDR} (recv buffer: {} KB)", granted / 1024);
    if granted < 1024 * 1024 {
        println!(
            "NOTE: OS gave only {} KB. On Linux raise it with: \
             sudo sysctl -w net.core.rmem_max=8388608",
            granted / 1024
        );
    }

    // ---- Python side runs in its own thread
    let (tx, rx) = sync_channel::<Vec<u8>>(4);
    thread::spawn(move || python_thread(rx));

    // ---- reassembly state
    let mut in_flight: HashMap<u32, FrameAssembly> = HashMap::new();
    let mut last_done: Option<u32> = None; // newest frame id already finished
    let mut last_activity = Instant::now();
    let mut stats = Stats::default();
    let mut stats_timer = Instant::now();

    let mut buffer = [0u8; 2048];

    // forwards a finished frame without ever blocking the UDP loop
    let forward = |frame: Vec<u8>, stats: &mut Stats| match tx.try_send(frame) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => stats.dropped_slow += 1,
        Err(TrySendError::Disconnected(_)) => stats.dropped_slow += 1,
    };

    loop {
        match socket.recv_from(&mut buffer) {
            Ok((size, _from)) => {
                last_activity = Instant::now();
                stats.datagrams += 1;

                if size < HEADER_LEN {
                    stats.bad_packets += 1;
                    continue;
                }

                let rd = |o: usize| {
                    u32::from_be_bytes([buffer[o], buffer[o + 1], buffer[o + 2], buffer[o + 3]])
                };
                let frame_id = rd(0);
                let total = rd(4);
                let packet_id = rd(8);

                if total == 0 || total > MAX_PACKETS_PER_FRAME || packet_id >= total {
                    stats.bad_packets += 1;
                    continue;
                }

                // late packet of a frame we already finished/abandoned
                if let Some(done) = last_done {
                    if !is_newer(frame_id, done) {
                        stats.stale += 1;
                        continue;
                    }
                }

                let asm = in_flight
                    .entry(frame_id)
                    .or_insert_with(|| FrameAssembly::new(total));

                if asm.total != total {
                    stats.bad_packets += 1; // header disagrees with earlier packets
                    continue;
                }

                let slot = &mut asm.slots[packet_id as usize];
                if slot.is_none() {
                    *slot = Some(buffer[HEADER_LEN..size].to_vec());
                    asm.received += 1;
                }

                if asm.is_complete() {
                    let frame = asm.build_complete();
                    in_flight.remove(&frame_id);

                    // everything older than this frame is now pointless
                    let older: Vec<u32> = in_flight
                        .keys()
                        .copied()
                        .filter(|id| is_newer(frame_id, *id))
                        .collect();
                    for id in older {
                        in_flight.remove(&id);
                        stats.lost += 1;
                    }
                    last_done = Some(frame_id);

                    if looks_like_jpeg(&frame) {
                        stats.complete += 1;
                        forward(frame, &mut stats);
                    } else {
                        stats.bad_jpeg += 1;
                    }
                }

                // never keep too many half-built frames
                while in_flight.len() > MAX_IN_FLIGHT {
                    let oldest = in_flight
                        .keys()
                        .copied()
                        .reduce(|a, b| if is_newer(a, b) { b } else { a })
                        .unwrap();
                    in_flight.remove(&oldest);
                    stats.lost += 1;
                    last_done = Some(match last_done {
                        Some(d) if is_newer(d, oldest) => d,
                        _ => oldest,
                    });
                }
            }

            Err(e) => match e.kind() {
                ErrorKind::WouldBlock
                | ErrorKind::TimedOut
                | ErrorKind::Interrupted
                | ErrorKind::ConnectionReset => {} // Windows reports ICMP errors here
                _ => return Err(e),
            },
        }

        // ---- time-based housekeeping (runs after every packet / 20 ms)

        // sender was restarted (frame ids start from 0 again)?
        if last_activity.elapsed() > IDLE_RESET {
            in_flight.clear();
            last_done = None;
        }

        // give up on frames that stopped receiving packets
        let expired: Vec<u32> = in_flight
            .iter()
            .filter(|(_, a)| a.first_seen.elapsed() > FRAME_TIMEOUT)
            .map(|(id, _)| *id)
            .collect();
        for id in expired {
            let asm = in_flight.remove(&id).unwrap();
            stats.lost += 1;
            last_done = Some(match last_done {
                Some(d) if is_newer(d, id) => d,
                _ => id,
            });
            if FORWARD_INCOMPLETE {
                forward(asm.build_zero_filled(), &mut stats);
            }
        }

        if stats_timer.elapsed() >= Duration::from_secs(1) {
            println!(
                "[1s] datagrams {:5} | frames ok {:3} | lost {:3} | bad_jpeg {} | \
                 dropped_slow {} | stale pkts {} | bad pkts {}",
                stats.datagrams, stats.complete, stats.lost, stats.bad_jpeg,
                stats.dropped_slow, stats.stale, stats.bad_packets
            );
            stats = Stats::default();
            stats_timer = Instant::now();
        }
    }
}
