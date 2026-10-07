import threading


class FrameBuffer:

    def __init__(self, buffer_size=60):
        # frame_id -> OpenCV frame
        self.frames = {}

        # Number of frames to collect before playback starts
        self.buffer_size = buffer_size

        # Protect buffer from receiver/player threads
        self.lock = threading.Lock()


    def add_frame(self, frame_id, frame):
        """
        Add a complete or recovered frame to the buffer.
        """

        with self.lock:
            self.frames[frame_id] = frame


    def get_frame(self, frame_id):
        """
        Get a frame using its frame ID.
        """

        with self.lock:
            return self.frames.get(frame_id)


    def remove_frame(self, frame_id):
        """
        Remove a frame after it has been played.
        """

        with self.lock:
            if frame_id in self.frames:
                del self.frames[frame_id]


    def get_oldest_frame_id(self):
        """
        Return the oldest frame currently in the buffer.
        """

        with self.lock:

            if not self.frames:
                return None

            return min(self.frames.keys())


    def is_ready(self):
        """
        Check whether enough frames have been
        collected to start playback.
        """

        with self.lock:
            return len(self.frames) >= self.buffer_size


    def size(self):
        """
        Return the current number of frames
        in the buffer.
        """

        with self.lock:
            return len(self.frames)


    def drop_older_than(self, frame_id):
        """
        Remove frames older than frame_id. Used for recovered frames
        that finished after playback had already moved past them.
        """

        with self.lock:

            old_ids = [
                fid for fid in self.frames
                if fid < frame_id
            ]

            for fid in old_ids:
                del self.frames[fid]
