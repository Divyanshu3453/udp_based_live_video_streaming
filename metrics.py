import threading
import time


class Metrics:
    """
    Thread-safe counters for the experiments.

    The receiver thread, the recovery worker thread and the player
    thread all update this object, so every update takes the lock.
    """

    def __init__(self):

        self.lock = threading.Lock()

        self.started_at = time.monotonic()

        # Frames
        self.complete_frames = 0
        self.missing_frames = 0

        # Recovery
        self.recovery_attempts = 0
        self.recovery_successes = 0
        self.recovery_failures = 0
        self.recovery_windows_discarded = 0
        self.recovery_times = []

        # Playback
        self.played_frames = 0
        self.buffer_underruns = 0
        self.playback_stalls = 0
        self.skipped_frames = 0
        self.repeated_frames = 0
        self.buffer_samples = []

    def add(self, name, amount=1):

        with self.lock:
            setattr(self, name, getattr(self, name) + amount)

    def add_recovery_time(self, seconds):

        with self.lock:
            self.recovery_times.append(seconds)

    def add_buffer_sample(self, size):

        with self.lock:
            self.buffer_samples.append(size)

    def summary(self):

        with self.lock:

            elapsed = time.monotonic() - self.started_at

            total_frames = self.complete_frames + self.missing_frames

            frame_loss = (
                100.0 * self.missing_frames / total_frames
                if total_frames
                else 0.0
            )

            recovery_success = (
                100.0 * self.recovery_successes / self.recovery_attempts
                if self.recovery_attempts
                else 0.0
            )

            if self.recovery_times:
                avg_ms = (
                    1000.0
                    * sum(self.recovery_times)
                    / len(self.recovery_times)
                )
                max_ms = 1000.0 * max(self.recovery_times)
            else:
                avg_ms = 0.0
                max_ms = 0.0

            if self.buffer_samples:
                avg_buffer = (
                    sum(self.buffer_samples)
                    / len(self.buffer_samples)
                )
            else:
                avg_buffer = 0.0

            lines = [
                "",
                "========== METRICS ==========",
                f"Run time:                {elapsed:.1f} s",
                f"Complete frames:         {self.complete_frames}",
                f"Missing frames:          {self.missing_frames}",
                f"Frame loss:              {frame_loss:.2f}%",
                f"Recovery attempts:       {self.recovery_attempts}",
                f"Successful recoveries:   {self.recovery_successes}",
                f"Failed recoveries:       {self.recovery_failures}",
                f"Recovery success:        {recovery_success:.1f}%",
                f"Windows discarded:       {self.recovery_windows_discarded}",
                f"RIFE time (avg / max):   {avg_ms:.1f} / {max_ms:.1f} ms",
                f"Played frames:           {self.played_frames}",
                f"Buffer underruns:        {self.buffer_underruns}",
                f"Playback stalls:         {self.playback_stalls}",
                f"Skipped frames:          {self.skipped_frames}",
                f"Repeated frames:         {self.repeated_frames}",
                f"Average buffer size:     {avg_buffer:.1f}",
                "=============================",
                "",
            ]

            return "\n".join(lines)


# One shared instance used by the whole Python side.
metrics = Metrics()
