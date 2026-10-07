from buffer import FrameBuffer
from reciever import receive_frames
from player import VideoPlayer
from metrics import metrics
import threading


 
buffer = FrameBuffer()


# Create the video player.
player = VideoPlayer(buffer)


# Receiver runs in its own thread.
receiver_thread = threading.Thread(
    target=receive_frames,
    args=(buffer,),
    daemon=True
)

receiver_thread.start()


# Player runs on the main thread.
player.play()


# Print the experiment results when playback ends.
print(metrics.summary())
