import time
import cv2

from metrics import metrics


# How long to wait for a missing frame (for example while RIFE is
# still recovering it) before giving up on it and moving on.
MISSING_FRAME_WAIT = 0.5

# Frame interval at 30 FPS, in milliseconds.
FRAME_INTERVAL_MS = 33


class VideoPlayer:

    def __init__(self, buffer):

        self.buffer = buffer

        # We don't know the starting frame yet.
        self.next_frame_id = None

        self.running = True

        # Last frame shown, repeated while a hole is being recovered.
        self.last_frame = None


    def show(self, frame, delay_ms):

        cv2.imshow("UDP Video", frame)

        key = cv2.waitKey(delay_ms) & 0xFF

        if key == ord("q"):
            self.running = False


    def play(self):

        print("Video player waiting for buffer...")


        # Wait until the buffer has enough frames.
        while self.running:

            if self.buffer.is_ready():

                break

            time.sleep(0.001)


        if not self.running:
            return


        # Start from the oldest frame.
        self.next_frame_id = (
            self.buffer.get_oldest_frame_id()
        )


        print(
            f"Buffer ready: "
            f"{self.buffer.size()} frames"
        )

        print(
            f"Starting playback from "
            f"frame {self.next_frame_id}"
        )


        # When we started waiting for the current frame.
        wait_started = None

        # Play faster than real time when the buffer is well above its
        # target, and at normal speed otherwise.
        fast_threshold = self.buffer.buffer_size + 5


        # Normal playback.
        while self.running:

            frame = self.buffer.get_frame(
                self.next_frame_id
            )


            # Next frame isn't available yet.
            if frame is None:

                if wait_started is None:

                    wait_started = time.monotonic()

                waited = time.monotonic() - wait_started

                oldest = self.buffer.get_oldest_frame_id()

                # Nothing buffered at all: a real underrun.
                if oldest is None:

                    if waited < 0.01:
                        metrics.add("buffer_underruns")

                    if self.last_frame is not None:
                        self.show(self.last_frame, 1)
                    else:
                        time.sleep(0.001)

                    continue


                # Later frames exist, so this one is a hole.
                # Keep the picture alive by repeating the last frame
                # while recovery finishes.
                if waited < MISSING_FRAME_WAIT:

                    if self.last_frame is not None:

                        self.show(
                            self.last_frame,
                            FRAME_INTERVAL_MS
                        )

                        metrics.add("repeated_frames")

                    else:

                        time.sleep(0.001)

                    continue


                # Recovery didn't finish in time: give up on this
                # frame and move to the next one.
                print(
                    f"Playback stall: gave up on "
                    f"frame {self.next_frame_id}"
                )

                metrics.add("playback_stalls")
                metrics.add("skipped_frames")

                self.next_frame_id += 1

                wait_started = None

                continue


            wait_started = None


            # Display frame.
            if self.buffer.size() > fast_threshold:
                delay = 1
            else:
                delay = FRAME_INTERVAL_MS

            self.show(frame, delay)

            self.last_frame = frame

            if not self.running:
                break


            # Frame has been played.
            self.buffer.remove_frame(
                self.next_frame_id
            )

            metrics.add("played_frames")
            metrics.add_buffer_sample(self.buffer.size())


            # Move to next frame.
            self.next_frame_id += 1

            # Discard recovered frames that arrived too late.
            self.buffer.drop_older_than(self.next_frame_id)


        cv2.destroyAllWindows()


    def stop(self):

        self.running = False
