import cv2
import socket
import struct
import numpy as np
import time


HOST = "127.0.0.1"
PORT = 9001


sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.connect((HOST, PORT))

print("Connected to Rust receiver")
print("Waiting for video frames...")


frame_count = 0
start_time = time.time()


while True:

    # ================================================
    # 1. RECEIVE HEADER
    # ================================================

    t1 = time.perf_counter()

    header = b""

    while len(header) < 4:
        data = sock.recv(4 - len(header))

        if not data:
            print("Rust disconnected")
            break

        header += data

    if len(header) < 4:
        break

    frame_size = struct.unpack("!I", header)[0]

    t2 = time.perf_counter()


    # ================================================
    # 2. RECEIVE JPEG
    # ================================================

    frame_data = bytearray()

    while len(frame_data) < frame_size:

        data = sock.recv(
            min(65536, frame_size - len(frame_data))
        )

        if not data:
            print("Rust disconnected")
            break

        frame_data.extend(data)

    if len(frame_data) < frame_size:
        break

    t3 = time.perf_counter()


    # ================================================
    # 3. JPEG → NUMPY
    # ================================================

    image_buffer = np.frombuffer(
        frame_data,
        dtype=np.uint8
    )


    # ================================================
    # 4. JPEG DECODE
    # ================================================

    frame = cv2.imdecode(
        image_buffer,
        cv2.IMREAD_COLOR
    )

    t4 = time.perf_counter()

    if frame is None:
        print("JPEG decode failed")
        continue


    # ================================================
    # 5. DISPLAY
    # ================================================

    cv2.imshow(
        "UDP Video Stream",
        frame
    )

    key = cv2.waitKey(1) & 0xFF

    t5 = time.perf_counter()


    # ================================================
    # TIMINGS
    # ================================================

    print(
        f"Frame {frame_count} | "
        f"Size: {frame_size / 1024:.1f} KB | "
        f"TCP recv: {(t3 - t1) * 1000:.2f} ms | "
        f"Decode: {(t4 - t3) * 1000:.2f} ms | "
        f"Display: {(t5 - t4) * 1000:.2f} ms"
    )


    frame_count += 1

    elapsed = time.time() - start_time

    if elapsed >= 1.0:

        print(
            f"========== FPS: {frame_count / elapsed:.2f} =========="
        )

        frame_count = 0
        start_time = time.time()


    if key == ord("q"):
        break


sock.close()
cv2.destroyAllWindows()