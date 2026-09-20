#!/usr/bin/env python3
"""Resolve crop and cut times for process.sh, printed as shell assignments.

    sync.py <recording.mov> <run-dir> <offset-seconds>
"""
import json, subprocess, sys
from datetime import datetime

mov, run_dir, offset = sys.argv[1], sys.argv[2], float(sys.argv[3])
probe = json.loads(subprocess.check_output([
    "ffprobe", "-v", "quiet", "-print_format", "json",
    "-show_format", "-show_streams", mov]))
video = next(s for s in probe["streams"] if s["codec_type"] == "video")
tags = {k.lower(): v for k, v in probe["format"].get("tags", {}).items()}
stamp = tags.get("com.apple.quicktime.creationdate") or tags.get("creation_time")
if not stamp:
    sys.exit("recording has no creation timestamp; cannot sync")
start = datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp() + offset

marks = {}
for line in open(f"{run_dir}/timeline.jsonl"):
    entry = json.loads(line)
    marks[entry["mark"]] = entry["t"] / 1000 - start

geo = json.load(open(f"{run_dir}/geometry.json"))
px_per_pt = int(video["width"]) / geo["display"]["width"]
w = geo["window"]
crop = [round(w["width"] * px_per_pt), round(w["height"] * px_per_pt),
        round(w["x"] * px_per_pt), round(w["y"] * px_per_pt)]
crop = [c - c % 2 for c in crop]

def t(name):
    value = marks[name]
    if value < 0:
        sys.exit(f"mark {name} is before the recording started ({value:.1f} s)")
    return f"{value:.2f}"

print(f"CROP={crop[0]}:{crop[1]}:{crop[2]}:{crop[3]}")
print(f"THEME={geo.get('theme', 'dark')}")
for name in ["agents-ready", "waiting", "hero-dark", "answered", "all-idle", "grid", "end"]:
    print(f"T_{name.replace('-', '_').upper()}={t(name)}")
# Stills: inside each hold, not on its edge.
print(f"S_HERO_DARK={float(t('hero-dark')) + 1.5:.2f}")
print(f"S_GRID={float(t('grid')) + 1.5:.2f}")
# The creation stamp is whole seconds, so a cut on `end` can reach the frame
# where the window is already gone; stop short of it.
print(f"T_CLIP_END={float(t('end')) - 1.5:.2f}")
