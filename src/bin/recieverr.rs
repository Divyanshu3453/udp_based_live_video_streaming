// ======================================================================
// UDP receiver -> frame reassembly -> TCP to Python
//
// Two threads:
//   * main thread   : ONLY reads UDP and reassembles frames (never blocks)
//   * python thread : ONLY writes frame events to Python over TCP
//
// TCP protocol to Python:
//
// COMPLETE FRAME:
//   [frame_id: 4 bytes]
//   [status:   1 byte = 0]
//   [length:   4 bytes]
//   [JPEG:     N bytes]
//
// MISSING FRAME:
//   [frame_id: 4 bytes]
//   [status:   1 byte = 1]
//   [length:   4 bytes = 0]
//
// This allows Python to know that a frame was actually lost instead of
// simply never receiving anything for that frame.
//
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

// UDP application header:
// frame_id     -> 4 bytes
// total_packets-> 4 bytes
// packet_id   -> 4 bytes
const HEADER_LEN: usize = 12;

const MAX_PAYLOAD: usize = 1200;

// Sanity limit.
// 20,000 packets × 1200 bytes ≈ 24 MB maximum frame.
const MAX_PACKETS_PER_FRAME: u32 = 20_000;

// Ask OS for an 8 MB UDP receive buffer.
const RECV_BUFFER_BYTES: usize = 8 * 1024 * 1024;

// Give up on a frame if packets stop arriving for this long.
const FRAME_TIMEOUT: Duration = Duration::from_millis(250);

// If no UDP traffic exists for this long, assume sender restarted.
const IDLE_RESET: Duration = Duration::from_secs(2);

// Never keep more than this many partially received frames.
const MAX_IN_FLIGHT: usize = 8;

// ----------------------------- types -----------------------------------

/// What Rust wants to send to Python.
///
/// Complete -> contains the reconstructed JPEG.
/// Missing  -> contains only the frame ID.
enum FrameStatus {
    Complete(Vec<u8>),
    Missing,
}

// ----------------------------- helpers --------------------------------

/// Returns true if frame `a` is newer than frame `b`.
///
/// The wrapping subtraction makes this safe when u32 frame IDs
/// eventually wrap around from 4,294,967,295 back to 0.
fn is_newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000_0000
}

/// A JPEG should start with FF D8 and end with FF D9.
///
/// IMPORTANT:
/// This only checks the JPEG markers. It does not guarantee that
/// OpenCV/Python can successfully decode the image.
fn looks_like_jpeg(data: &[u8]) -> bool {
    data.len() > 4
        && data[0] == 0xFF
        && data[1] == 0xD8
        && data[data.len() - 2] == 0xFF
        && data[data.len() - 1] == 0xD9
}

// -------------------------- frame assembly -----------------------------

struct FrameAssembly {
    // Expected number of UDP packets for this frame.
    total: u32,

    // One slot for every packet.
    //
    // Example:
    //
    // packet 0 -> Some(...)
    // packet 1 -> Some(...)
    // packet 2 -> None       <- missing
    // packet 3 -> Some(...)
    //
    slots: Vec<Option<Vec<u8>>>,

    // Number of packets successfully received.
    received: u32,

    // When we first saw a packet belonging to this frame.
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

    /// Returns true when every packet has arrived.
    fn is_complete(&self) -> bool {
        self.received == self.total
    }

    /// Reconstruct the original JPEG.
    ///
    /// Only call this when the frame is complete.
    fn build_complete(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(
            self.total as usize * MAX_PAYLOAD
        );

        for slot in &self.slots {
            out.extend_from_slice(
                slot.as_ref().unwrap()
            );
        }

        out
    }
}

// ------------------------------ stats ---------------------------------

#[derive(Default)]
struct Stats {
    // Successfully reconstructed JPEG frames.
    complete: u32,

    // Frames abandoned because packets were missing.
    lost: u32,

    // Complete frame did not look like a JPEG.
    bad_jpeg: u32,

    // Python side could not consume frames quickly enough.
    dropped_slow: u32,

    // Packets belonging to frames already finished/abandoned.
    stale: u32,

    // Malformed or inconsistent UDP packets.
    bad_packets: u32,

    // Total UDP datagrams received.
    datagrams: u32,
}

// ------------------------ TCP -> Python thread ------------------------

/// Sends frame events to Python over TCP.
///
/// IMPORTANT:
/// This thread performs ALL TCP writes.
///
/// Therefore the UDP receiving thread never waits for Python.
fn python_thread(rx: Receiver<(u32, FrameStatus)>) {
    let listener = TcpListener::bind(PYTHON_ADDR)
        .expect("bind TCP 9001");

    println!(
        "Waiting for Python display on {PYTHON_ADDR} ..."
    );

    loop {
        let (mut stream, addr) = match listener.accept() {
            Ok(v) => v,

            Err(e) => {
                println!("accept failed: {e}");
                continue;
            }
        };

        println!(
            "Python display connected from {addr}"
        );

        // Disable Nagle's algorithm.
        let _ = stream.set_nodelay(true);

        // --------------------------------------------------------------
        // If frames accumulated while Python was disconnected,
        // discard them.
        // --------------------------------------------------------------

        while rx.try_recv().is_ok() {}

        // --------------------------------------------------------------
        // Process frame events.
        // --------------------------------------------------------------

        for (frame_id, status) in rx.iter() {
            let mut out = Vec::new();

            match status {
                // ------------------------------------------------------
                // COMPLETE
                // ------------------------------------------------------

                FrameStatus::Complete(frame) => {
                    // frame_id
                    out.extend_from_slice(
                        &frame_id.to_be_bytes()
                    );

                    // status = 0
                    out.push(0);

                    // JPEG length
                    out.extend_from_slice(
                        &(frame.len() as u32).to_be_bytes()
                    );

                    // JPEG data
                    out.extend_from_slice(&frame);
                }

                // ------------------------------------------------------
                // MISSING
                // ------------------------------------------------------

                FrameStatus::Missing => {
                    // frame_id
                    out.extend_from_slice(
                        &frame_id.to_be_bytes()
                    );

                    // status = 1
                    out.push(1);

                    // No JPEG data.
                    out.extend_from_slice(
                        &0u32.to_be_bytes()
                    );
                }
            }

            // ----------------------------------------------------------
            // One TCP write for the complete event.
            // ----------------------------------------------------------

            if let Err(e) = stream.write_all(&out) {
                println!(
                    "Python disconnected: {e}"
                );

                break;
            }
        }
    }
}

// ------------------------------- main ---------------------------------

fn main() -> std::io::Result<()> {

    // ==================================================================
    // UDP SOCKET
    // ==================================================================

    // socket2 is used so that we can request a large receive buffer.
    let socket = Socket::new(
        Domain::IPV4,
        Type::DGRAM,
        Some(Protocol::UDP),
    )?;

    // Request 8 MB receive buffer.
    let _ = socket.set_recv_buffer_size(
        RECV_BUFFER_BYTES
    );

    // Bind UDP socket.
    socket.bind(
        &UDP_ADDR
            .parse::<std::net::SocketAddr>()
            .unwrap()
            .into()
    )?;

    // Convert socket2 socket into std UdpSocket.
    let socket: UdpSocket = socket.into();

    // Don't block forever waiting for UDP.
    //
    // This allows housekeeping to run every ~20 ms.
    socket.set_read_timeout(
        Some(Duration::from_millis(20))
    )?;

    // Check how much buffer the OS actually granted.
    let granted = socket2::SockRef::from(&socket)
        .recv_buffer_size()
        .unwrap_or(0);

    println!(
        "UDP listening on {UDP_ADDR} \
         (recv buffer: {} KB)",
        granted / 1024
    );

    if granted < 1024 * 1024 {
        println!(
            "NOTE: OS gave only {} KB. On Linux raise it with: \
             sudo sysctl -w net.core.rmem_max=8388608",
            granted / 1024
        );
    }

    // ==================================================================
    // TCP -> PYTHON CHANNEL
    // ==================================================================

    // The channel contains:
    //
    // (frame_id, FrameStatus)
    //
    // It is bounded so that Python cannot make memory grow forever.
    let (tx, rx) =
        sync_channel::<(u32, FrameStatus)>(4);

    // Start TCP writer thread.
    thread::spawn(move || {
        python_thread(rx);
    });

    // ==================================================================
    // FRAME REASSEMBLY STATE
    // ==================================================================

    // Multiple frames can be partially received at the same time.
    //
    // frame_id -> FrameAssembly
    let mut in_flight:
        HashMap<u32, FrameAssembly> = HashMap::new();

    // Newest frame that has already been completed or declared missing.
    let mut last_done: Option<u32> = None;

    // Last time ANY UDP packet was received.
    let mut last_activity = Instant::now();

    // Statistics.
    let mut stats = Stats::default();

    // Used for printing statistics every second.
    let mut stats_timer = Instant::now();

    // One UDP datagram can contain:
    //
    // 12-byte header
    // +
    // up to 1200-byte payload
    //
    // So 2048 bytes is safely enough.
    let mut buffer = [0u8; 2048];

    // ==================================================================
    // FORWARD HELPER
    // ==================================================================

    // This function NEVER blocks.
    //
    // If Python is too slow and the channel is full,
    // we simply discard the event rather than stopping UDP reception.
    let forward =
        |frame_id: u32,
         status: FrameStatus,
         stats: &mut Stats| {
            match tx.try_send((frame_id, status)) {

                Ok(()) => {}

                Err(TrySendError::Full(_)) => {
                    stats.dropped_slow += 1;
                }

                Err(TrySendError::Disconnected(_)) => {
                    stats.dropped_slow += 1;
                }
            }
        };

    // ==================================================================
    // MAIN UDP LOOP
    // ==================================================================

    loop {

        match socket.recv_from(&mut buffer) {

            // ==========================================================
            // UDP DATAGRAM RECEIVED
            // ==========================================================

            Ok((size, _from)) => {

                last_activity = Instant::now();

                stats.datagrams += 1;

                // ------------------------------------------------------
                // Basic packet size validation.
                // ------------------------------------------------------

                if size < HEADER_LEN {
                    stats.bad_packets += 1;
                    continue;
                }

                // ------------------------------------------------------
                // Read our custom UDP header.
                //
                // byte 0..4  -> frame_id
                // byte 4..8  -> total_packets
                // byte 8..12 -> packet_id
                // ------------------------------------------------------

                let rd = |o: usize| {
                    u32::from_be_bytes([
                        buffer[o],
                        buffer[o + 1],
                        buffer[o + 2],
                        buffer[o + 3],
                    ])
                };

                let frame_id = rd(0);
                let total = rd(4);
                let packet_id = rd(8);

                // ------------------------------------------------------
                // Validate packet metadata.
                // ------------------------------------------------------

                if total == 0
                    || total > MAX_PACKETS_PER_FRAME
                    || packet_id >= total
                {
                    stats.bad_packets += 1;
                    continue;
                }

                // ------------------------------------------------------
                // Ignore packets belonging to a frame that has already
                // been completed or declared missing.
                // ------------------------------------------------------

                if let Some(done) = last_done {
                    if !is_newer(frame_id, done) {
                        stats.stale += 1;
                        continue;
                    }
                }

                // ------------------------------------------------------
                // Get/create assembly for this frame.
                // ------------------------------------------------------

                let asm = in_flight
                    .entry(frame_id)
                    .or_insert_with(|| {
                        FrameAssembly::new(total)
                    });

                // ------------------------------------------------------
                // Make sure every packet agrees on total_packets.
                // ------------------------------------------------------

                if asm.total != total {
                    stats.bad_packets += 1;
                    continue;
                }

                // ------------------------------------------------------
                // Store packet payload.
                //
                // Duplicate packets are ignored.
                // ------------------------------------------------------

                let slot =
                    &mut asm.slots[packet_id as usize];

                if slot.is_none() {

                    *slot = Some(
                        buffer[HEADER_LEN..size]
                            .to_vec()
                    );

                    asm.received += 1;
                }

                // ======================================================
                // FRAME COMPLETE
                // ======================================================

                if asm.is_complete() {

                    // Reconstruct JPEG.
                    let frame =
                        asm.build_complete();

                    // Remove from in-flight map.
                    in_flight.remove(&frame_id);

                    // --------------------------------------------------
                    // Any older incomplete frames are now pointless.
                    //
                    // Example:
                    //
                    // 100 complete
                    // 101 incomplete
                    // 102 complete
                    //
                    // When 102 arrives, 101 is declared missing.
                    // --------------------------------------------------

                    let older: Vec<u32> =
                        in_flight
                            .keys()
                            .copied()
                            .filter(|id| {
                                is_newer(
                                    frame_id,
                                    *id
                                )
                            })
                            .collect();

                    for id in older {

                        in_flight.remove(&id);

                        stats.lost += 1;

                        // Tell Python that this exact frame is missing.
                        forward(
                            id,
                            FrameStatus::Missing,
                            &mut stats,
                        );
                    }

                    // This is now the newest finished frame.
                    last_done = Some(frame_id);

                    // --------------------------------------------------
                    // Validate JPEG.
                    // --------------------------------------------------

                    if looks_like_jpeg(&frame) {

                        stats.complete += 1;

                        // Send the complete frame to Python.
                        forward(
                            frame_id,
                            FrameStatus::Complete(frame),
                            &mut stats,
                        );

                    } else {

                        stats.bad_jpeg += 1;
                    }
                }

                // ======================================================
                // LIMIT NUMBER OF PARTIAL FRAMES
                // ======================================================

                while in_flight.len() > MAX_IN_FLIGHT {

                    // Find oldest frame.
                    let oldest =
                        in_flight
                            .keys()
                            .copied()
                            .reduce(|a, b| {
                                if is_newer(a, b) {
                                    b
                                } else {
                                    a
                                }
                            })
                            .unwrap();

                    // Remove it.
                    in_flight.remove(&oldest);

                    stats.lost += 1;

                    // Update newest completed/abandoned frame.
                    last_done = Some(
                        match last_done {
                            Some(d)
                                if is_newer(d, oldest) =>
                            {
                                d
                            }

                            _ => oldest,
                        }
                    );

                    // Tell Python this frame is missing.
                    forward(
                        oldest,
                        FrameStatus::Missing,
                        &mut stats,
                    );
                }
            }

            // ==========================================================
            // UDP RECEIVE ERROR / TIMEOUT
            // ==========================================================

            Err(e) => {

                match e.kind() {

                    ErrorKind::WouldBlock
                    | ErrorKind::TimedOut
                    | ErrorKind::Interrupted
                    | ErrorKind::ConnectionReset => {
                        // Normal.
                        //
                        // Windows can report ICMP-related errors
                        // as ConnectionReset.
                    }

                    _ => return Err(e),
                }
            }
        }

        // ==================================================================
        // HOUSEKEEPING
        // ==================================================================

        // --------------------------------------------------------------
        // Detect sender restart.
        //
        // If there has been no UDP traffic for >2 seconds,
        // frame IDs may restart from 0.
        // --------------------------------------------------------------

        if last_activity.elapsed() > IDLE_RESET {

            in_flight.clear();

            last_done = None;
        }

        // --------------------------------------------------------------
        // Find frames that have timed out.
        // --------------------------------------------------------------

        let expired: Vec<u32> =
            in_flight
                .iter()
                .filter(|(_, assembly)| {
                    assembly.first_seen.elapsed()
                        > FRAME_TIMEOUT
                })
                .map(|(id, _)| *id)
                .collect();

        // --------------------------------------------------------------
        // Declare expired frames MISSING.
        // --------------------------------------------------------------

        for id in expired {

            // Remove incomplete assembly.
            in_flight.remove(&id);

            stats.lost += 1;

            // Update last finished frame.
            last_done = Some(
                match last_done {
                    Some(d) if is_newer(d, id) => d,
                    _ => id,
                }
            );

            // ----------------------------------------------------------
            // IMPORTANT:
            //
            // We DO NOT create a fake/zero-filled JPEG.
            //
            // We explicitly tell Python:
            //
            // "frame ID X is missing."
            //
            // Python can later put this frame into the ML recovery
            // pipeline.
            // ----------------------------------------------------------

            forward(
                id,
                FrameStatus::Missing,
                &mut stats,
            );
        }

        // ==================================================================
        // PRINT STATISTICS EVERY SECOND
        // ==================================================================

        if stats_timer.elapsed()
            >= Duration::from_secs(1)
        {
            println!(
                "[1s] datagrams {:5} | \
                 frames ok {:3} | \
                 lost {:3} | \
                 bad_jpeg {} | \
                 dropped_slow {} | \
                 stale pkts {} | \
                 bad pkts {}",

                stats.datagrams,
                stats.complete,
                stats.lost,
                stats.bad_jpeg,
                stats.dropped_slow,
                stats.stale,
                stats.bad_packets,
            );

            stats = Stats::default();

            stats_timer = Instant::now();
        }
    }
}