use std::collections::HashMap;
use std::io::{ErrorKind, Write};
use std::net::{TcpListener, UdpSocket};
use std::sync::mpsc::{sync_channel, Receiver, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

use rand::RngExt;
use socket2::{Domain, Protocol, Socket, Type};

const UDP_ADDR: &str = "0.0.0.0:8000";
const PYTHON_ADDR: &str = "127.0.0.1:9001";

const HEADER_LEN: usize = 12;
const MAX_PAYLOAD: usize = 1200;
const MAX_PACKETS_PER_FRAME: u32 = 20_000;

const RECV_BUFFER_BYTES: usize = 8 * 1024 * 1024;

const FRAME_TIMEOUT: Duration =
    Duration::from_millis(250);

const IDLE_RESET: Duration =
    Duration::from_secs(2);

const MAX_IN_FLIGHT: usize = 8;


// Random loss testing

// Random packet loss probability.
// 0.005 = 0.5% of received UDP packets will be dropped.
//
// Experiment stages:
//   Experiment 1: 0.0 (zero loss, RIFE should never be called)
//   Experiment 2: 0.005
//   Experiment 3: 0.005 plus burst loss
//   Experiment 4: 0.01, 0.02, ... (increase gradually)
//
// Remember that one lost packet makes the whole JPEG unusable, so frame
// loss is much higher than packet loss (about 1 - (1 - p)^packets_per_frame).
const RANDOM_LOSS_RATE: f64 = 0.0002;

// Enable burst loss testing.
const BURST_LOSS_ENABLED: bool = true;

// Probability of starting a burst.
const BURST_START_RATE: f64 = 0.0007;

// Number of consecutive packets dropped during a burst.
const BURST_LENGTH: u32 = 150;


// Frame status

enum FrameStatus {
    Complete(Vec<u8>),
    Missing,
}


// Frame id comparison

fn is_newer(a: u32, b: u32) -> bool {
    a != b && a.wrapping_sub(b) < 0x8000_0000
}


// Jpeg check

fn looks_like_jpeg(data: &[u8]) -> bool {
    data.len() > 4
        && data[0] == 0xFF
        && data[1] == 0xD8
        && data[data.len() - 2] == 0xFF
        && data[data.len() - 1] == 0xD9
}


// Frame assembly

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


// Statistics

#[derive(Default)]
struct Stats {

    complete: u32,
    lost: u32,

    bad_jpeg: u32,

    dropped_slow: u32,

    stale: u32,
    bad_packets: u32,

    datagrams: u32,

    // Artificial packets dropped by our tester
    test_dropped: u32,
}


// Random loss tester

struct LossTester {

    rng: rand::rngs::ThreadRng,

    burst_remaining: u32,
}

impl LossTester {

    fn new() -> Self {

        Self {
            rng: rand::rng(),
            burst_remaining: 0,
        }
    }


    // Returns true when this packet should be
    // artificially dropped.
    fn should_drop(&mut self) -> bool {

        // 1. Continue an existing burst

        if self.burst_remaining > 0 {

            self.burst_remaining -= 1;

            return true;
        }


        // 2. Randomly start a burst

        if BURST_LOSS_ENABLED
            && self.rng.random::<f64>()
                < BURST_START_RATE
        {

            // Current packet is part of the burst.
            self.burst_remaining =
                BURST_LENGTH.saturating_sub(1);

            return true;
        }


        // 3. Normal random packet loss

        if self.rng.random::<f64>()
            < RANDOM_LOSS_RATE
        {
            return true;
        }


        false
    }
}


// Python thread

fn python_thread(
    rx: Receiver<(u32, FrameStatus)>
) {

    let listener =
        TcpListener::bind(PYTHON_ADDR)
            .expect("bind TCP 9001");


    println!(
        "Waiting for Python display on {} ...",
        PYTHON_ADDR
    );


    loop {

        let (mut stream, addr) =
            match listener.accept() {

                Ok(v) => v,

                Err(e) => {

                    println!(
                        "accept failed: {e}"
                    );

                    continue;
                }
            };


        println!(
            "Python display connected from {addr}"
        );


        let _ =
            stream.set_nodelay(true);


        // Clear stale messages from previous
        // Python connection.
        while rx.try_recv().is_ok() {}


        for (frame_id, status) in rx.iter() {

            let mut out = Vec::new();


            match status {

                // Complete

                FrameStatus::Complete(frame) => {

                    out.extend_from_slice(
                        &frame_id.to_be_bytes()
                    );

                    // status = 0
                    out.push(0);

                    out.extend_from_slice(
                        &(frame.len() as u32)
                            .to_be_bytes()
                    );

                    out.extend_from_slice(
                        &frame
                    );
                }


                // Missing

                FrameStatus::Missing => {

                    out.extend_from_slice(
                        &frame_id.to_be_bytes()
                    );

                    // status = 1
                    out.push(1);

                    out.extend_from_slice(
                        &0u32.to_be_bytes()
                    );
                }
            }


            if let Err(e) =
                stream.write_all(&out)
            {

                println!(
                    "Python disconnected: {e}"
                );

                break;
            }
        }
    }
}


// Main

fn main() -> std::io::Result<()> {


    // Create udp socket

    let socket = Socket::new(
        Domain::IPV4,
        Type::DGRAM,
        Some(Protocol::UDP),
    )?;


    // Request a large OS receive buffer.
    let _ =
        socket.set_recv_buffer_size(
            RECV_BUFFER_BYTES
        );


    socket.bind(
        &UDP_ADDR
            .parse::<std::net::SocketAddr>()
            .unwrap()
            .into()
    )?;


    let socket: UdpSocket =
        socket.into();


    // Small timeout allows us to periodically
    // check frame expiration.
    socket.set_read_timeout(
        Some(Duration::from_millis(20))
    )?;


    let granted =
        socket2::SockRef::from(&socket)
            .recv_buffer_size()
            .unwrap_or(0);


    println!(
        "UDP listening on {} \
         (recv buffer: {} KB)",
        UDP_ADDR,
        granted / 1024
    );


    if granted < 1024 * 1024 {

        println!(
            "NOTE: OS gave only {} KB. \
             On Linux raise it with: \
             sudo sysctl -w \
             net.core.rmem_max=8388608",
            granted / 1024
        );
    }


    // Random loss tester

    let mut loss_tester =
        LossTester::new();


    println!();
    println!("========== LOSS TEST ==========");
    println!(
        "Random packet loss: {}%",
        RANDOM_LOSS_RATE * 100.0
    );
    println!(
        "Burst loss: {}",
        BURST_LOSS_ENABLED
    );

    if BURST_LOSS_ENABLED {

        println!(
            "Burst start probability: {}%",
            BURST_START_RATE * 100.0
        );

        println!(
            "Burst length: {} packets",
            BURST_LENGTH
        );
    }

    println!("================================");
    println!();


    // Python channel

    let (tx, rx) =
        sync_channel::<(u32, FrameStatus)>(4);


    thread::spawn(move || {

        python_thread(rx);
    });


    // Frame state

    let mut in_flight:
        HashMap<u32, FrameAssembly> =
            HashMap::new();


    let mut last_done:
        Option<u32> = None;


    let mut last_activity =
        Instant::now();


    let mut stats =
        Stats::default();


    let mut stats_timer =
        Instant::now();


    let mut buffer =
        [0u8; 2048];


    // Forward to python

    let forward =
        |frame_id: u32,
         status: FrameStatus,
         stats: &mut Stats| {

            match tx.try_send(
                (frame_id, status)
            ) {

                Ok(()) => {}

                Err(
                    TrySendError::Full(_)
                ) => {

                    stats.dropped_slow += 1;
                }

                Err(
                    TrySendError::Disconnected(_)
                ) => {

                    stats.dropped_slow += 1;
                }
            }
        };


    // Main udp loop

    loop {


        match socket.recv_from(
            &mut buffer
        ) {


            // Packet received

            Ok((size, _from)) => {

                last_activity =
                    Instant::now();

                stats.datagrams += 1;


                // Artificial packet loss

                if loss_tester.should_drop() {

                    stats.test_dropped += 1;

                    continue;
                }


                // Header check

                if size < HEADER_LEN {

                    stats.bad_packets += 1;

                    continue;
                }


                // Read header

                let rd = |o: usize| {

                    u32::from_be_bytes([
                        buffer[o],
                        buffer[o + 1],
                        buffer[o + 2],
                        buffer[o + 3],
                    ])
                };


                let frame_id =
                    rd(0);


                let total =
                    rd(4);


                let packet_id =
                    rd(8);


                // Validate header

                if total == 0
                    || total >
                        MAX_PACKETS_PER_FRAME
                    || packet_id >= total
                {

                    stats.bad_packets += 1;

                    continue;
                }


                // Ignore old frames

                if let Some(done) =
                    last_done
                {

                    if !is_newer(
                        frame_id,
                        done
                    ) {

                        stats.stale += 1;

                        continue;
                    }
                }


                // Get frame assembly

                let asm =
                    in_flight
                        .entry(frame_id)
                        .or_insert_with(|| {

                            FrameAssembly::new(
                                total
                            )
                        });


                // Total packet count must match

                if asm.total != total {

                    stats.bad_packets += 1;

                    continue;
                }


                // Store packet

                let slot =
                    &mut asm.slots[
                        packet_id as usize
                    ];


                // Avoid duplicate packets.
                if slot.is_none() {

                    *slot = Some(
                        buffer[
                            HEADER_LEN..size
                        ].to_vec()
                    );

                    asm.received += 1;
                }


                // Frame complete

                if asm.is_complete() {


                    let frame =
                        asm.build_complete();


                    in_flight.remove(
                        &frame_id
                    );


                    // Any older incomplete frames?

                    let older:
                        Vec<u32> =

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

                        in_flight.remove(
                            &id
                        );

                        stats.lost += 1;


                        forward(
                            id,
                            FrameStatus::Missing,
                            &mut stats
                        );
                    }


                    last_done =
                        Some(frame_id);


                    // Jpeg validation

                    if looks_like_jpeg(
                        &frame
                    ) {

                        stats.complete += 1;


                        forward(
                            frame_id,
                            FrameStatus::Complete(
                                frame
                            ),
                            &mut stats
                        );

                    } else {

                        stats.bad_jpeg += 1;
                    }
                }


                // Too many frames in flight

                while in_flight.len()
                    > MAX_IN_FLIGHT
                {


                    let oldest =
                        in_flight
                            .keys()
                            .copied()
                            .reduce(
                                |a, b| {

                                    if is_newer(
                                        a,
                                        b
                                    ) {

                                        b

                                    } else {

                                        a
                                    }
                                }
                            )
                            .unwrap();


                    in_flight.remove(
                        &oldest
                    );


                    stats.lost += 1;


                    last_done =
                        Some(
                            match last_done {

                                Some(d)
                                    if is_newer(
                                        d,
                                        oldest
                                    ) =>
                                {
                                    d
                                }

                                _ =>
                                    oldest,
                            }
                        );


                    forward(
                        oldest,
                        FrameStatus::Missing,
                        &mut stats
                    );
                }
            }


            // Socket timeout / error

            Err(e) => {

                match e.kind() {

                    ErrorKind::WouldBlock
                    | ErrorKind::TimedOut
                    | ErrorKind::Interrupted
                    | ErrorKind::ConnectionReset => {}


                    _ => return Err(e),
                }
            }
        }


        // Reset after long idle

        if last_activity.elapsed()
            > IDLE_RESET
        {

            in_flight.clear();

            last_done = None;
        }


        // Frame timeout

        let expired:
            Vec<u32> =

            in_flight
                .iter()
                .filter(
                    |(_, assembly)| {

                        assembly
                            .first_seen
                            .elapsed()
                            > FRAME_TIMEOUT
                    }
                )
                .map(
                    |(id, _)| *id
                )
                .collect();


        for id in expired {

            in_flight.remove(
                &id
            );


            stats.lost += 1;


            last_done =
                Some(
                    match last_done {

                        Some(d)
                            if is_newer(
                                d,
                                id
                            ) =>
                        {
                            d
                        }

                        _ => id,
                    }
                );


            forward(
                id,
                FrameStatus::Missing,
                &mut stats
            );
        }


        // Print statistics every second

        if stats_timer.elapsed()
            >= Duration::from_secs(1)
        {

            let packet_loss_pct = if stats.datagrams > 0 {
                100.0 * stats.test_dropped as f64 / stats.datagrams as f64
            } else {
                0.0
            };

            let frames_total = stats.complete + stats.lost;
            let frame_loss_pct = if frames_total > 0 {
                100.0 * stats.lost as f64 / frames_total as f64
            } else {
                0.0
            };

            println!(
                "[1s] packet loss {:.2}% | frame loss {:.1}%",
                packet_loss_pct, frame_loss_pct
            );

            println!(
                "[1s] datagrams {:5} | \
                 frames ok {:3} | \
                 lost {:3} | \
                 test_drop {:3} | \
                 bad_jpeg {} | \
                 dropped_slow {} | \
                 stale pkts {} | \
                 bad pkts {}",
                stats.datagrams,
                stats.complete,
                stats.lost,
                stats.test_dropped,
                stats.bad_jpeg,
                stats.dropped_slow,
                stats.stale,
                stats.bad_packets,
            );


            stats =
                Stats::default();


            stats_timer =
                Instant::now();
        }
    }
}