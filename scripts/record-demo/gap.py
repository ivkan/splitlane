#!/usr/bin/env python3
"""Find the stretch of the host scene where no window is on screen.

    gap.py <recording.mov> <crop w:h:x:y> <t-close> <t-closed> <t-reopened>

Prints GAP_START, GAP_END (seconds into the recording) and GROUND (the
window's rail colour, as 0xRRGGBB), as shell assignments for process.sh.

The timeline says when the script asked the window to close and when it saw
the next one up, not when either showed on screen, and the recording's own
start is known to a second at best. So the gap is read from the pictures:
a frame taken a second after the window's process had ended shows only what
is behind the window, and the gap is every frame, around it, that still
looks like that one.
"""
import subprocess
import sys

W, H = 48, 32  # small enough to compare quickly, large enough to see a window


def frames(mov, crop, start, end, fps=10):
    """(time, pixels) for each frame of the crop between start and end."""
    raw = subprocess.check_output([
        "ffmpeg", "-v", "error", "-ss", f"{start:.3f}", "-to", f"{end:.3f}", "-i", mov,
        "-vf", f"crop={crop},fps={fps},scale={W}:{H}:flags=area", "-f", "rawvideo",
        "-pix_fmt", "rgb24", "-"])
    size = W * H * 3
    return [(start + i / fps, raw[i * size:(i + 1) * size]) for i in range(len(raw) // size)]


def distance(a, b):
    return sum(abs(x - y) for x, y in zip(a, b)) / len(a)


def main():
    mov, crop = sys.argv[1], sys.argv[2]
    t_close, t_closed, t_reopened = (float(v) for v in sys.argv[3:6])
    window = frames(mov, crop, max(0.0, t_close - 1.0), t_close - 0.5)
    behind = frames(mov, crop, t_closed + 1.0, t_closed + 1.2)
    if not window or not behind:
        sys.exit("gap.py: the recording does not cover the close")
    window, behind = window[0][1], behind[0][1]
    # Halfway between the two pictures: a frame is "no window" when it is
    # nearer what is behind than what the window showed.
    threshold = distance(window, behind) / 2
    if threshold < 4:
        sys.exit("gap.py: the frame after the close looks like the window; is the timeline in sync?")
    span = frames(mov, crop, max(0.0, t_close - 0.5), t_reopened)
    empty = [t for t, px in span if distance(px, behind) < threshold]
    if not empty:
        sys.exit("gap.py: no frame without the window between close and reopen")
    # The first and last such frame; a frame's span is a tenth of a second.
    print(f"GAP_START={min(empty):.2f}")
    print(f"GAP_END={max(empty) + 0.1:.2f}")
    # The rail's colour: the left edge, halfway down, of the last frame with
    # the window. Painting the gap in it reads as the window's own ground.
    x, y = 1, H // 2
    r, g, b = window[(y * W + x) * 3:(y * W + x) * 3 + 3]
    print(f"GROUND=0x{r:02X}{g:02X}{b:02X}")


if __name__ == "__main__":
    main()
