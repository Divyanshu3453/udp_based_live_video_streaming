import cv2
import socket
import struct

HOST = "127.0.0.1"
PORT = 9000

sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
sock.connect((HOST, PORT))

camera = cv2.VideoCapture(0)

if not camera.isOpened():
    print("Could not open camera")
    exit()

print("Connected to Rust")
print("Press Q to stop")

frame_id = 0

while True:
    success, frame = camera.read()

    if not success:
        print("Could not read frame")
        break

    # Encode frame as JPEG
    success, encoded = cv2.imencode(".jpg", frame)

    if not success:
        continue

    frame_bytes = encoded.tobytes()

    # 4-byte unsigned integer containing frame size
    header = struct.pack("!I", len(frame_bytes))

    # Send length + complete frame
    sock.sendall(header)
    sock.sendall(frame_bytes)

    print(
        f"Frame {frame_id} sent: "
        f"{len(frame_bytes)} bytes"
    )

    frame_id += 1

    cv2.imshow("Python Camera", frame)

    if cv2.waitKey(1) & 0xFF == ord("q"):
        break

camera.release()
sock.close()
cv2.destroyAllWindows()