import queue
import socket
import struct
import threading
import time

from construction import construct_frame
from frame_recovery.frame_recover import frame_recovery, warm_up
from metrics import metrics


HOST = "127.0.0.1"
PORT = 9001

# Largest missing window handed to RIFE in one go.
# At 30 FPS, 30 frames is about 1 second.
# Longer gaps are discarded and we wait for a new valid reference.
MAX_PENDING_MISSING = 30

# Recovery jobs waiting for the RIFE worker.
# If RIFE cannot keep up, new jobs are dropped instead of
# blocking the socket and starving the Rust receiver.
RECOVERY_QUEUE_SIZE = 8


def recv_exact(sock, size):
    data = bytearray()

    while len(data) < size:

        chunk = sock.recv(
            min(65536, size - len(data))
        )

        if not chunk:
            return None

        data.extend(chunk)

    return data


def recovery_worker(jobs, buffer):
    """
    Runs RIFE in its own thread so that inference time never blocks
    reading frames from Rust.

    Each job is (previous_frame, next_frame, missing_ids).
    """

    # Load RIFE once, before the first real missing frame.
    try:
        warm_up()
    except Exception as error:
        print(f"RIFE warm-up failed: {error}")

    while True:

        job = jobs.get()

        if job is None:
            break

        previous_frame, next_frame, missing_ids = job

        total_missing = len(missing_ids)

        for position, missing_id in enumerate(missing_ids, start=1):

            metrics.add("recovery_attempts")

            start = time.perf_counter()

            recovered_frame = frame_recovery(
                previous_frame,
                next_frame,
                position,
                total_missing
            )

            metrics.add_recovery_time(time.perf_counter() - start)

            if recovered_frame is not None:

                buffer.add_frame(missing_id, recovered_frame)

                metrics.add("recovery_successes")

                print(f"Frame {missing_id} | RECOVERED -> BUFFER")

            else:

                metrics.add("recovery_failures")

                print(f"Frame {missing_id} | RECOVERY FAILED")


def receive_frames(buffer):

    sock = socket.socket(
        socket.AF_INET,
        socket.SOCK_STREAM
    )

    sock.connect((HOST, PORT))

    print("Connected to Rust receiver")
    print("Waiting for frames...")

    jobs = queue.Queue(maxsize=RECOVERY_QUEUE_SIZE)

    worker = threading.Thread(
        target=recovery_worker,
        args=(jobs, buffer),
        daemon=True
    )

    worker.start()

    # Last successfully received complete frame
    previous_frame = None
    previous_frame_id = None

    # Number of MISSING reports from Rust since the last complete frame
    # (used for logging and metrics only; the recovery window itself is
    # computed from frame ids, see below).
    reported_missing = 0

    while True:

        # Read 9-byte Rust -> Python message header
        #
        # [0..4] : frame_id     (4 bytes)
        # [4]    : status       (1 byte)
        # [5..9] : frame_size   (4 bytes)
        #
        # status:
        # 0 = COMPLETE
        # 1 = MISSING

        header = recv_exact(sock, 9)

        if header is None:
            print("Rust disconnected")
            break

        frame_id = struct.unpack("!I", header[0:4])[0]

        status = header[4]

        frame_size = struct.unpack("!I", header[5:9])[0]

        # Missing frame

        if status == 1:

            print(f"Frame {frame_id} | MISSING")

            reported_missing += 1

            continue

        # Unknown status

        if status != 0:

            print(f"Unknown frame status: {status}")

            break

        # Complete frame

        frame_data = recv_exact(sock, frame_size)

        if frame_data is None:

            print("Rust disconnected while receiving JPEG")

            break

        # JPEG -> OpenCV frame

        frame = construct_frame(frame_data)

        if frame is None:

            print(f"Frame {frame_id} | JPEG decode failed")

            continue

        metrics.add("complete_frames")

        # Work out which frames lie between the previous complete frame
        # and this one.
        #
        # Using frame ids (instead of only the MISSING messages) also
        # covers frames that lost every packet and so were never
        # reported by Rust.

        if previous_frame is None:

            # No temporal reference yet, so nothing can be recovered.
            if reported_missing:
                print(
                    f"Cannot recover {reported_missing} "
                    f"frame(s): no previous complete frame"
                )

        elif frame_id > previous_frame_id + 1:

            missing_ids = list(
                range(previous_frame_id + 1, frame_id)
            )

            metrics.add("missing_frames", len(missing_ids))

            if len(missing_ids) > MAX_PENDING_MISSING:

                print(
                    f"{len(missing_ids)} consecutive missing frames "
                    f"(limit {MAX_PENDING_MISSING}). "
                    f"Discarding recovery window."
                )

                metrics.add("recovery_windows_discarded")

            else:

                print(
                    f"Recovering {len(missing_ids)} missing frame(s) "
                    f"using frames {previous_frame_id} and {frame_id}"
                )

                try:

                    jobs.put_nowait(
                        (previous_frame, frame, missing_ids)
                    )

                except queue.Full:

                    print("Recovery queue full. Window dropped.")

                    metrics.add("recovery_windows_discarded")

        reported_missing = 0

        # Add the real complete frame to the buffer

        buffer.add_frame(frame_id, frame)

        print(
            f"Frame {frame_id} | COMPLETE -> BUFFER | "
            f"Buffer size: {buffer.size()}"
        )

        # This becomes the previous temporal frame
        # for the next recovery window.

        previous_frame = frame
        previous_frame_id = frame_id

    jobs.put(None)

    sock.close()
