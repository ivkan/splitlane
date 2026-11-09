#!/usr/bin/env python3
"""Resolve crop and cut times for process.sh, printed as shell assignments.

    sync.py <recording.mov> <run-dir> <offset-seconds>
"""
import json, shlex, subprocess, sys
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
# Every mark, for the edit: seconds into the recording, which is also seconds
# into the uncut window.mp4. Marks from before the recording started are left
# out rather than refused - the edit does not need them all.
cue = {name: round(value, 2) for name, value in marks.items() if value >= 0}
cue["_window"] = {"width": crop[0], "height": crop[1]}
print(f"MARKS_JSON={shlex.quote(json.dumps(cue))}")
print(f"THEME={geo.get('theme', 'dark')}")
scene = geo.get("scene", "main")
print(f"SCENE={scene}")
# The marks each scene's cut needs; a run missing one cannot be cut.
NEEDED = {
    "main": ["agents-ready", "prompts-sent", "waiting", "hero-dark", "jump", "answered",
             "all-idle", "answer-sent", "grid", "files", "end"],
    "fleet": ["agents-ready", "prompt-sent", "all-opened", "waiting", "fleet-hold", "end"],
    "host": ["agents-ready", "prompts-sent", "close", "closed", "reopen", "sessions-back", "end"],
}
for name in NEEDED.get(scene, []):
    if name not in marks:
        sys.exit(f"the timeline has no mark {name}; the {scene} run did not finish")
for name in NEEDED.get(scene, []):
    print(f"T_{name.replace('-', '_').upper()}={t(name)}")
# The creation stamp is whole seconds, so a cut on `end` can reach the frame
# where the window is already gone; stop short of it.
print(f"T_CLIP_END={float(t('end')) - 1.5:.2f}")
# Stills: inside each hold, not on its edge.
if scene == "main":
    print(f"S_HERO_DARK={float(t('hero-dark')) + 1.5:.2f}")
    print(f"S_GRID={float(t('grid')) + 1.5:.2f}")
if scene == "fleet":
    print(f"S_FLEET={float(t('fleet-hold')) + 2:.2f}")
