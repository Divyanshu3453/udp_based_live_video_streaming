import socket
import struct
import time
import cv2

from construction import construct_frame
from frame_recovery.frame_recover import frame_recovery


HOST = "127.0.0.1"
PORT = 9001


# Connect to the Rust receiver over TCP
sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.connect((HOST, PORT))

print("Connected to Rust receiver")
print("Waiting for video frames...")


# Statistics
frame_count = 0
start_time = time.time()

complete_count = 0
missing_count = 0
recovered_count = 0


# Store the previous complete frame.
# It will be used as temporal context for recovery.
previous_frame = None
previous_frame_id = None

# Store the ID of a missing frame until its next frame arrives.
pending_missing_id = None


def recv_exact(sock, size):
    """
    Receive exactly `size` bytes from TCP.

    TCP is a byte stream, so one recv() call
    may not contain all the requested bytes.
    """

    data = bytearray()

    while len(data) < size:

        chunk = sock.recv(
            min(65536, size - len(data))
        )

        if not chunk:
            return None

        data.extend(chunk)

    return data


while True:

    t1 = time.perf_counter()

    # Rust sends a 9-byte frame event header:
    # 4 bytes frame_id
    # 1 byte status
    # 4 bytes frame_size
    header = recv_exact(sock, 9)

    if header is None:
        print("Rust disconnected")
        break


    # Extract frame ID from the header
    frame_id = struct.unpack(
        "!I",
        header[0:4]
    )[0]

    # 0 = complete frame
    # 1 = missing frame
    status = header[4]

    # Extract JPEG size
    frame_size = struct.unpack(
        "!I",
        header[5:9]
    )[0]


    # Handle a missing frame
    if status == 1:

        missing_count += 1

        print(
            f"Frame {frame_id} | MISSING"
        )

        # We cannot recover immediately because we
        # need the next complete frame as context.
        pending_missing_id = frame_id

        continue


    # Handle an unknown status value
    if status != 0:

        print(
            f"Unknown frame status: {status}"
        )

        break


    # Receive the JPEG data for a complete frame
    frame_data = recv_exact(
        sock,
        frame_size
    )

    if frame_data is None:

        print(
            "Rust disconnected while receiving JPEG"
        )

        break


    t2 = time.perf_counter()


    # Convert JPEG bytes into an OpenCV frame
    frame = construct_frame(frame_data)

    t3 = time.perf_counter()


    # JPEG construction failed
    if frame is None:

        print(
            f"Frame {frame_id} | "
            f"JPEG decode failed"
        )

        continue


    # If an earlier frame was missing, we now have
    # both the previous and next complete frames.
    if pending_missing_id is not None:

        print(
            f"Recovering missing frame "
            f"{pending_missing_id} "
            f"using frames "
            f"{previous_frame_id} and {frame_id}"
        )

        recovered_frame = frame_recovery(
            previous_frame,
            frame
        )

        if recovered_frame is not None:

            recovered_count += 1

            print(
                f"Frame {pending_missing_id} | "
                f"RECOVERED"
            )

            # Temporarily display the recovered frame.
            # Later this will go through the playout system.
            cv2.imshow(
                "UDP Video Stream",
                recovered_frame
            )

            cv2.waitKey(1)

        pending_missing_id = None


    # Display the current complete frame
    cv2.imshow(
        "UDP Video Stream",
        frame
    )

    key = cv2.waitKey(1) & 0xFF


    # Current frame becomes the previous frame
    # for possible future recovery.
    previous_frame = frame
    previous_frame_id = frame_id


    complete_count += 1
    frame_count += 1

    print(
        f"Frame {frame_id} | "
        f"COMPLETE | "
        f"Size: {frame_size / 1024:.1f} KB | "
        f"TCP recv: {(t2 - t1) * 1000:.2f} ms | "
        f"Construction: {(t3 - t2) * 1000:.2f} ms"
    )


    # Print statistics approximately once per second
    elapsed = time.time() - start_time

    if elapsed >= 1.0:

        print(
            f"FPS: {frame_count / elapsed:.2f} | "
            f"Complete: {complete_count} | "
            f"Missing: {missing_count} | "
            f"Recovered: {recovered_count}"
        )

        frame_count = 0
        complete_count = 0
        missing_count = 0
        recovered_count = 0

        start_time = time.time()


    if key == ord("q"):
        break


sock.close()
cv2.destroyAllWindows()