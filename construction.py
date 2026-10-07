import cv2
import numpy as np


def construct_frame(frame_data):
    """
    Convert JPEG bytes received from Rust
    into an OpenCV frame.
    """

    image_buffer = np.frombuffer(
        frame_data,
        dtype=np.uint8
    )

    frame = cv2.imdecode(
        image_buffer,
        cv2.IMREAD_COLOR
    )

    return frame