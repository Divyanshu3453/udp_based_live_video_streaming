
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



# Statistics


frame_count = 0
start_time = time.time()

complete_count = 0
missing_count = 0



# Helper: receive exactly N bytes

def recv_exact(sock, size):
    """
    Receive exactly `size` bytes from TCP.

    TCP is a byte stream, so one recv() call is NOT guaranteed
    to return all requested bytes.
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



# Main loop


while True:

 
    # 1. RECEIVE FRAME EVENT HEADER
    #
    # Rust sends:
    #
    # frame_id : 4 bytes
    # status   : 1 byte
    # length   : 4 bytes
    #
    # Total = 9 bytes
  

    t1 = time.perf_counter()

    header = recv_exact(sock, 9)

    if header is None:

        print("Rust disconnected")
        break

   
    # Extracting  frame ID
   

    frame_id = struct.unpack(
        "!I",
        header[0:4]
    )[0]


    # Extract status
    #
    # 0 = COMPLETE
    # 1 = MISSING
  

    status = header[4]

   
    # Extract JPEG length


    frame_size = struct.unpack(
        "!I",
        header[5:9]
    )[0]

    t2 = time.perf_counter()


  
    # 2. MISSING FRAME


    if status == 1:

        missing_count += 1

        print(
            f"Frame {frame_id} | "
            f"MISSING"
        )

    
        # IMPORTANT:
        #
        # There is NO JPEG data for this frame.
        #
        # Later this is where we will put:
        #
        #     frame_buffer[frame_id] = MISSING
        #
        # and eventually:
        #
        #     RIFE / GAN recovery
        #
      

        continue


    # 3. COMPLETE FRAME
  
    if status != 0:

        print(
            f"Unknown frame status: {status}"
        )

        break


   

    frame_data = recv_exact(
        sock,
        frame_size
    )

    if frame_data is None:

        print("Rust disconnected while receiving JPEG")
        break

    t3 = time.perf_counter()


   
    image_buffer = np.frombuffer(
        frame_data,
        dtype=np.uint8
    )


   
   

    frame = cv2.imdecode(
        image_buffer,
        cv2.IMREAD_COLOR
    )

    t4 = time.perf_counter()


    if frame is None:

        print(
            f"Frame {frame_id} | "
            f"JPEG decode failed"
        )

        continue


  
    cv2.imshow(
        "UDP Video Stream",
        frame
    )

    key = cv2.waitKey(1) & 0xFF

    t5 = time.perf_counter()


   
    # 8. STATISTICS
   

    complete_count += 1
    frame_count += 1


    print(
        f"Frame {frame_id} | "
        f"COMPLETE | "
        f"Size: {frame_size / 1024:.1f} KB | "
        f"TCP recv: {(t3 - t1) * 1000:.2f} ms | "
        f"Decode: {(t4 - t3) * 1000:.2f} ms | "
        f"Display: {(t5 - t4) * 1000:.2f} ms"
    )


   
    # 9. FPS
  

    elapsed = time.time() - start_time

    if elapsed >= 1.0:

        print(
            f"========== "
            f"FPS: {frame_count / elapsed:.2f} "
            f"| Complete: {complete_count} "
            f"| Missing: {missing_count} "
            f"=========="
        )

        frame_count = 0
        complete_count = 0
        missing_count = 0

        start_time = time.time()


   
   
 

    if key == ord("q"):
        break




sock.close()
cv2.destroyAllWindows()

