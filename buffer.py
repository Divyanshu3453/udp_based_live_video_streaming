import threading


class FrameBuffer:

    def __init__(self):
        # frame_id -> frame information
        self.frames = {}

        # Multiple threads will access the buffer.
        self.lock = threading.Lock()


    def add_frame(self, frame_id, frame):
        """
        Add a complete frame to the buffer.
        """

        with self.lock:

            self.frames[frame_id] = {
                "frame": frame,
                "status": "complete"
            }


    def add_missing(self, frame_id):
        """
        Add a missing frame to the buffer.
        """

        with self.lock:

            self.frames[frame_id] = {
                "frame": None,
                "status": "missing"
            }


    def add_recovered(self, frame_id, frame):
        """
        Replace a missing frame with its recovered frame.
        """

        with self.lock:

            self.frames[frame_id] = {
                "frame": frame,
                "status": "recovered"
            }


    def get_frame(self, frame_id):
        """
        Get frame information using frame ID.
        """

        with self.lock:

            return self.frames.get(frame_id)


    def has_frame(self, frame_id):
        """
        Check whether a frame ID exists in the buffer.
        """

        with self.lock:

            return frame_id in self.frames


    def remove_frame(self, frame_id):
        """
        Remove a frame after it has been played.
        """

        with self.lock:

            if frame_id in self.frames:
                del self.frames[frame_id]


    def size(self):
        """
        Return the number of frames currently stored.
        """

        with self.lock:

            return len(self.frames)