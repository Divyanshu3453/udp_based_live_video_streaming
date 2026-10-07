import inspect
import os
import sys
import time

import cv2
import numpy as np
import torch
from torch.nn import functional as F


# Locate Practical-RIFE.
# Expected layout:
#   <project>/Practical-RIFE/train_log/...   (model files)
#   <project>/frame_recovery/frame_recover.py (this file)

CURRENT_DIR = os.path.dirname(os.path.abspath(__file__))

RIFE_ROOT = os.path.abspath(
    os.path.join(CURRENT_DIR, "..", "Practical-RIFE")
)

if RIFE_ROOT not in sys.path:
    sys.path.insert(0, RIFE_ROOT)


DEVICE = torch.device(
    "cuda" if torch.cuda.is_available() else "cpu"
)

# Bisection depth used only when the loaded model cannot take an
# arbitrary timestep (older RIFE models only produce the midpoint).
MAX_BISECTION_DEPTH = 4

# RIFE runs on frames no wider than this, and the result is scaled back
# up to the original size. Inference cost grows with pixel count, so this
# is the main speed knob on a CPU. Set to 0 to run at full resolution.
RIFE_MAX_WIDTH = 320


# The model is loaded only once and then kept in memory.
# We do NOT load it every time a frame is missing.

_model = None
_supports_timestep = False


def _import_model_class():
    """
    Practical-RIFE ships its model in train_log/RIFE_HDv3.py.
    The original RIFE repository uses model/RIFE_HDv2.py.
    Try the Practical-RIFE layout first.
    """

    try:
        from train_log.RIFE_HDv3 import Model
        return Model
    except Exception:
        pass

    from model.RIFE_HDv2 import Model
    return Model


def load_rife():

    global _model
    global _supports_timestep

    if _model is not None:
        return _model

    print()
    print("Loading RIFE...")

    try:

        Model = _import_model_class()

        model = Model()

        model.load_model(
            os.path.join(RIFE_ROOT, "train_log"),
            -1
        )

    except Exception as error:

        print("Failed to load RIFE model.")
        print(f"Error: {error}")

        raise

    if not hasattr(model, "version"):
        model.version = 0

    model.eval()
    model.device()

    # Newer Practical-RIFE models accept a timestep argument:
    #   inference(img0, img1, timestep, scale)
    # Older ones only support the midpoint.
    try:
        parameters = inspect.signature(model.inference).parameters
        _supports_timestep = "timestep" in parameters
    except (TypeError, ValueError):
        _supports_timestep = False

    _model = model

    print(f"RIFE device: {DEVICE}")
    print(f"RIFE version: {model.version}")
    print(f"Arbitrary timestep supported: {_supports_timestep}")
    print("RIFE ready.")
    print()

    return _model


def warm_up(height=480, width=640):
    """
    Load the model and run one dummy inference so the first real
    recovery does not pay the initialization cost.
    """

    dummy = np.zeros((height, width, 3), dtype=np.uint8)

    frame_recovery(dummy, dummy, 1, 1)


# Convert OpenCV frame -> RIFE tensor (BGR uint8 -> RGB float).

def frame_to_tensor(frame):

    rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)

    tensor = torch.from_numpy(rgb).permute(2, 0, 1).float() / 255.0

    tensor = tensor.unsqueeze(0).to(DEVICE)

    return tensor


# Convert RIFE tensor -> OpenCV frame (RGB float -> BGR uint8).

def tensor_to_frame(tensor, height, width):

    frame = (
        tensor[0]
        .clamp(0, 1)
        .cpu()
        .numpy()
        .transpose(1, 2, 0)
    )

    frame = (frame * 255.0).astype(np.uint8)

    frame = frame[:height, :width]

    frame = cv2.cvtColor(frame, cv2.COLOR_RGB2BGR)

    return frame


def _interpolate(model, img0, img1, ratio):
    """
    Return the frame at temporal position `ratio` (0..1)
    between img0 and img1.
    """

    if _supports_timestep:
        return model.inference(img0, img1, ratio)

    # Fallback for midpoint-only models: repeatedly take midpoints
    # and move towards the requested position.
    low, high = img0, img1
    t_low, t_high = 0.0, 1.0

    middle = None

    for _ in range(MAX_BISECTION_DEPTH):

        middle = model.inference(low, high)
        t_middle = (t_low + t_high) / 2.0

        if abs(ratio - t_middle) < 1e-3:
            break

        if ratio < t_middle:
            high, t_high = middle, t_middle
        else:
            low, t_low = middle, t_middle

    return middle


# Actual RIFE recovery.
#
# Example:
#
#   100 COMPLETE
#   101 MISSING
#   102 MISSING
#   103 MISSING
#   104 COMPLETE
#
#   total_missing = 3
#
#   101 -> 1/4
#   102 -> 2/4
#   103 -> 3/4

def frame_recovery(
    previous_frame,
    next_frame,
    position,
    total_missing
):

    # Safety checks

    if previous_frame is None:
        print("Recovery skipped: previous frame is None")
        return None

    if next_frame is None:
        print("Recovery skipped: next frame is None")
        return None

    if total_missing <= 0:
        print("Recovery skipped: invalid total_missing")
        return None

    if position <= 0 or position > total_missing:
        print("Recovery skipped: invalid position")
        return None

    if previous_frame.shape != next_frame.shape:
        print("Recovery skipped: frame dimensions do not match")
        return None

    ratio = position / (total_missing + 1)

    model = load_rife()

    height, width = previous_frame.shape[:2]

    # Shrink both frames before inference (see RIFE_MAX_WIDTH).
    work0 = previous_frame
    work1 = next_frame
    work_h, work_w = height, width

    if RIFE_MAX_WIDTH and width > RIFE_MAX_WIDTH:

        work_w = RIFE_MAX_WIDTH
        work_h = max(1, int(round(height * RIFE_MAX_WIDTH / width)))

        work0 = cv2.resize(
            previous_frame, (work_w, work_h),
            interpolation=cv2.INTER_AREA
        )
        work1 = cv2.resize(
            next_frame, (work_w, work_h),
            interpolation=cv2.INTER_AREA
        )

    img0 = frame_to_tensor(work0)
    img1 = frame_to_tensor(work1)

    # RIFE needs dimensions that are a multiple of 64.

    _, _, h, w = img0.shape

    padded_h = ((h - 1) // 64 + 1) * 64
    padded_w = ((w - 1) // 64 + 1) * 64

    padding = (0, padded_w - w, 0, padded_h - h)

    img0 = F.pad(img0, padding)
    img1 = F.pad(img1, padding)

    # Neural-network inference

    try:

        start = time.perf_counter()

        with torch.inference_mode():
            output = _interpolate(model, img0, img1, ratio)

        if DEVICE.type == "cuda":
            torch.cuda.synchronize()

        elapsed_ms = (time.perf_counter() - start) * 1000.0

    except Exception as error:

        print("RIFE inference failed:")
        print(error)

        return None

    if output is None:
        return None

    print(
        f"RIFE {position}/{total_missing} "
        f"(ratio {ratio:.2f}) took {elapsed_ms:.1f} ms"
    )

    recovered = tensor_to_frame(output, work_h, work_w)

    # Scale back up to the original frame size.
    if (work_h, work_w) != (height, width):
        recovered = cv2.resize(
            recovered, (width, height),
            interpolation=cv2.INTER_LINEAR
        )

    return recovered
