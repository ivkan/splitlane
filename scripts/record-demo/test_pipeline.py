#!/usr/bin/env python3
"""Dry checks of the recording pipeline: everything that can be checked
without opening a window, signing an agent in or recording the screen.

    python3 -I scripts/record-demo/test_pipeline.py [-v]

- scene.sh, for every scene: what it seeds, what it allows each agent, and
  that it writes nothing outside the stage.
- process.sh, for every scene, on a synthetic recording (ffmpeg's test
  pattern with a flat "window" on it) with a timeline and geometry as the
  record scripts write them: which assets come out, how long each clip is,
  and, for the host scene, that the stretch with no window is found and
  painted.
- lib.sh's stage_host_pids, stop_stage_hosts and stop_stage_processes: they
  pick out a stage's session host by the socket it listens on, and anything
  started from inside the stage (Codex's daemon) by its path, and leave
  another stage's alone.

Needs ffmpeg, ffprobe and magick, as process.sh does; shellcheck is used
when it is installed.
"""
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
from datetime import datetime, timezone

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
SCRIPTS = ["lib.sh", "scene.sh", "record.sh", "record-fleet.sh", "record-host.sh", "process.sh"]

# The synthetic screen: 320x200 points at 2 px per point, the window a
# 144x100-point rectangle at (40, 20) - the main window's 1.44:1.
DISPLAY = {"width": 320, "height": 200, "scale": 2}
WINDOW = {"x": 40, "y": 20, "width": 144, "height": 100}
WINDOW_COLOUR = "0x1E2227"
STAMP = datetime(2026, 11, 9, 10, 0, 0, tzinfo=timezone.utc)


def run(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw)


def duration(path):
    out = run(["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", path])
    return float(out.stdout)


def frame_rgb(path, at, w=8, h=8):
    raw = subprocess.check_output(["ffmpeg", "-v", "error", "-ss", f"{at:.2f}", "-i", path, "-frames:v", "1",
                                   "-vf", f"scale={w}:{h}:flags=area", "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
    n = len(raw) // 3
    return tuple(sum(raw[i::3]) / n for i in range(3))


def synthetic_run(tmp, scene, theme, marks, length, gap=None):
    """A recording and a run dir as record*.sh and the person leave them.

    `marks` are seconds into the recording; `gap` a (start, end) stretch in
    which the window is not drawn.
    """
    run_dir = os.path.join(tmp, f"run-{scene}-{theme}")
    os.makedirs(run_dir)
    epoch = STAMP.timestamp()
    with open(os.path.join(run_dir, "timeline.jsonl"), "w") as f:
        for name, sec in marks.items():
            f.write(json.dumps({"mark": name, "t": int((epoch + sec) * 1000)}) + "\n")
    with open(os.path.join(run_dir, "geometry.json"), "w") as f:
        json.dump({"scene": scene, "theme": theme, "window": WINDOW, "display": DISPLAY}, f)
    px = DISPLAY["scale"]
    box = f"drawbox=x={WINDOW['x'] * px}:y={WINDOW['y'] * px}:w={WINDOW['width'] * px}:h={WINDOW['height'] * px}:color={WINDOW_COLOUR}:t=fill"
    if gap:
        box += f":enable='not(between(t,{gap[0]},{gap[1]}))'"
    # A moving counter inside the window, so its frames are not all alike.
    text = f"drawbox=x={WINDOW['x'] * px + 20}:y={WINDOW['y'] * px + 20}:w=40:h=20:color=0x3A7BD5:t=fill"
    if gap:
        text += f":enable='not(between(t,{gap[0]},{gap[1]}))'"
    mov = os.path.join(run_dir, "recording.mov")
    run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i",
         f"testsrc2=size={DISPLAY['width'] * px}x{DISPLAY['height'] * px}:rate=30:duration={length}",
         "-vf", f"{box},{text}", "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p",
         "-metadata", f"creation_time={STAMP.strftime('%Y-%m-%dT%H:%M:%S.000000Z')}", mov])
    return mov, run_dir


def process(mov, run_dir, env=None):
    return run([os.path.join(HERE, "process.sh"), mov, run_dir], env={**os.environ, **(env or {})})


class Lint(unittest.TestCase):
    def test_scripts_parse(self):
        for name in SCRIPTS:
            run(["bash", "-n", os.path.join(HERE, name)])
        for name in ["sync.py", "gap.py", "rpc.py"]:
            run([sys.executable, "-m", "py_compile", os.path.join(HERE, name)])

    @unittest.skipUnless(shutil.which("shellcheck"), "shellcheck not installed")
    def test_shellcheck(self):
        run(["shellcheck", "-x", *SCRIPTS], cwd=HERE)


class Scene(unittest.TestCase):
    def stage(self, scene, codex="0", theme="dark"):
        tmp = tempfile.mkdtemp(prefix="sl-scene-")
        self.addCleanup(shutil.rmtree, tmp, True)
        # A HOME of its own, empty: scene.sh must leave it so.
        sentinel = os.path.join(tmp, "real-home")
        os.makedirs(sentinel)
        env = {"PATH": os.environ["PATH"], "HOME": sentinel, "DEMO_SCENE": scene,
               "DEMO_CODEX": codex, "DEMO_THEME": theme}
        repo = run([os.path.join(HERE, "scene.sh"), os.path.join(tmp, "stage")], env=env).stdout.strip()
        self.assertEqual(os.listdir(sentinel), [], "scene.sh wrote into HOME")
        home = os.path.join(tmp, "stage", "home")
        self.assertEqual(repo, os.path.join(os.path.realpath(home), "code", "ledger"))
        return os.path.realpath(home), repo

    def read(self, home, *parts):
        with open(os.path.join(home, *parts)) as f:
            return f.read()

    def session(self, home):
        return json.loads(self.read(home, "Library", "Caches", "splitlane-dev", "session-dev.json"))

    def app_config(self, home):
        return json.loads(self.read(home, "Library", "Application Support", "splitlane-dev", "splitlane.json"))

    def window(self, home):
        return json.loads(self.read(home, "Library", "Application Support", "splitlane", "window-state.json"))

    def allowed(self, home):
        return json.loads(self.read(home, ".claude", "settings.json"))["permissions"]["allow"]

    def test_main_three_claude(self):
        home, repo = self.stage("main")
        surfaces = self.session(home)["projects"][0]["surfaces"]
        self.assertEqual([s["agent"]["agent"] for s in surfaces], ["claude_code"] * 3)
        self.assertTrue(all(len(s["agent"]["session_id"]) == 36 for s in surfaces))
        self.assertEqual(len(self.session(home)["projects"][0]["layout"]["children"]), 3)
        self.assertIs(self.app_config(home)["session_host"], False)
        self.assertEqual(self.window(home), {"width": 1440, "height": 1000})
        self.assertNotIn("Bash", " ".join(self.allowed(home)))
        self.assertFalse(os.path.exists(os.path.join(home, ".codex")))
        self.assertFalse(os.path.exists(os.path.join(home, ".claude", "skills")))
        # The work in progress the diff shows before any agent acts.
        status = run(["git", "-C", repo, "status", "--porcelain"]).stdout
        self.assertEqual(status.strip(), "M src/parse.js")

    def test_main_with_codex_in_the_middle(self):
        home, repo = self.stage("main", codex="1")
        surfaces = self.session(home)["projects"][0]["surfaces"]
        self.assertEqual([s["agent"]["agent"] for s in surfaces], ["claude_code", "codex", "claude_code"])
        self.assertNotIn("session_id", surfaces[1]["agent"])
        config = self.read(home, ".codex", "config.toml")
        self.assertIn(f'[projects."{repo}"]\ntrust_level = "trusted"', config)
        self.assertIn('approval_policy = "never"', config)
        self.assertIn('sandbox_mode = "workspace-write"', config)
        # The cheapest model, never the account's default.
        self.assertIn('model = "gpt-6-luna"\nmodel_reasoning_effort = "low"', config)
        # The five hooks Splitlane's shim installs are trusted up front, with
        # the hash Codex 0.158 itself wrote for such a hook after "Trust all"
        # (one recorded case: a SessionStart hook `/bin/echo hi`, timeout 5).
        state = tomllib.loads(config)["hooks"]["state"]
        self.assertEqual(sorted(k.rsplit(":", 3)[1] for k in state),
                         ["post_tool_use", "pre_tool_use", "session_start", "stop", "user_prompt_submit"])
        self.assertTrue(all(k.startswith(f"{repo}/.codex/hooks.json:") and k.endswith(":0:0") for k in state))
        identity = {"event_name": "session_start",
                    "hooks": [{"type": "command", "command": "/bin/echo hi", "timeout": 5, "async": False}]}
        self.assertEqual(hashlib.sha256(json.dumps(identity, sort_keys=True, separators=(",", ":")).encode()).hexdigest(),
                         "b6d0c924fb913e6930ed38a1e6f4f573fbb2fddfcf5faed999af78a783a105f0")
        # The sign-in is lib.sh's to place, never scene.sh's.
        self.assertFalse(os.path.exists(os.path.join(home, ".codex", "auth.json")))

    def test_fleet_one_lead_with_the_skill(self):
        home, _ = self.stage("fleet", codex="1")  # Codex is never part of it
        surfaces = self.session(home)["projects"][0]["surfaces"]
        self.assertEqual(len(surfaces), 1)
        self.assertEqual(surfaces[0]["title"], "plan")
        self.assertIs(surfaces[0]["agent"]["title_user_set"], True)
        self.assertIn("Bash(splitlane:*)", self.allowed(home))
        self.assertNotIn("Bash", [a for a in self.allowed(home) if a != "Bash(splitlane:*)"])
        skill = self.read(home, ".claude", "skills", "splitlane-fleet", "SKILL.md")
        with open(os.path.join(ROOT, "skills", "splitlane-fleet", "SKILL.md")) as f:
            self.assertEqual(skill, f.read())
        self.assertEqual(self.window(home), {"width": 1600, "height": 824})
        self.assertAlmostEqual(1600 / 824, 1920 / 988, places=2)
        self.assertFalse(os.path.exists(os.path.join(home, ".codex")))

    def test_host_keeps_sessions(self):
        home, _ = self.stage("host", codex="1")
        config = self.app_config(home)
        self.assertIs(config["session_host"], True)
        self.assertEqual(config["on_quit"], "keep")
        self.assertEqual(len(self.session(home)["projects"][0]["surfaces"]), 3)

    def test_light_theme(self):
        home, _ = self.stage("fleet", theme="light")
        self.assertEqual(self.app_config(home)["theme"], "Harbor Light")
        self.assertEqual(json.loads(self.read(home, ".claude.json"))["theme"], "light")

    def test_unknown_scene_refused(self):
        tmp = tempfile.mkdtemp(prefix="sl-scene-")
        self.addCleanup(shutil.rmtree, tmp, True)
        result = subprocess.run([os.path.join(HERE, "scene.sh"), tmp], capture_output=True, text=True,
                                env={"PATH": os.environ["PATH"], "HOME": tmp, "DEMO_SCENE": "nope"})
        self.assertEqual(result.returncode, 2)


@unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("magick"), "needs ffmpeg and magick")
class Process(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="sl-process-")
        self.addCleanup(shutil.rmtree, self.tmp, True)

    def assets(self, run_dir):
        return sorted(os.listdir(os.path.join(run_dir, "assets")))

    def test_main_light(self):
        m = {"start": 0.5, "app-ready": 1, "agents-ready": 2, "prompts-sent": 6, "focus-walk": 9,
             "waiting": 21, "hero-dark": 25, "jump": 31, "answered": 34, "all-idle": 40,
             "answer-menu": 41.5, "answer-send": 44, "answer-sent": 48, "grid": 57, "files": 66,
             "palette": 72, "tour-end": 75, "history": 100, "history-end": 105, "end": 108}
        mov, run_dir = synthetic_run(self.tmp, "main", "light", m, 111)
        process(mov, run_dir)
        self.assertEqual(self.assets(run_dir), ["grid-diff-light.png", "grid.mp4", "hero-light.png", "hero.mp4",
                                                "history-light.png", "history.mp4",
                                                "marks.json", "state.mp4", "window.mp4"])
        a = os.path.join(run_dir, "assets")
        # 4 s of prompts at speed, 15 s of work at three times, 10 s of question.
        self.assertAlmostEqual(duration(os.path.join(a, "hero.mp4")), 4 + 15 / 3 + 10, delta=0.3)
        self.assertAlmostEqual(duration(os.path.join(a, "state.mp4")), duration(os.path.join(a, "hero.mp4")), delta=0.1)
        self.assertAlmostEqual(duration(os.path.join(a, "grid.mp4")), 66 - 48, delta=0.3)
        # The pointer arriving (1.5 s before the mark) to the end of the hover.
        self.assertAlmostEqual(duration(os.path.join(a, "history.mp4")), 105 - 98.5, delta=0.3)
        size = run(["ffprobe", "-v", "error", "-select_streams", "v", "-show_entries", "stream=width,height",
                    "-of", "csv=p=0", os.path.join(a, "hero.mp4")]).stdout.strip()
        self.assertEqual(size, "1920,1334")
        state = run(["ffprobe", "-v", "error", "-select_streams", "v", "-show_entries", "stream=width,height",
                     "-of", "csv=p=0", os.path.join(a, "state.mp4")]).stdout.strip()
        self.assertEqual(state, "960,666")
        with open(os.path.join(a, "marks.json")) as f:
            marks = json.load(f)
        self.assertAlmostEqual(marks["waiting"], 21, delta=0.01)

    def test_main_dark_keeps_the_readme_assets(self):
        m = {"start": 0.5, "agents-ready": 1, "prompts-sent": 3, "waiting": 9, "hero-dark": 11, "jump": 15,
             "answered": 17, "all-idle": 20, "answer-sent": 22, "grid": 26, "files": 30, "tour-end": 34,
             "history": 50, "history-end": 55, "end": 58}
        mov, run_dir = synthetic_run(self.tmp, "main", "dark", m, 60)
        process(mov, run_dir)
        for name in ["hero.mp4", "state.mp4", "grid.mp4", "demo.mp4", "demo.gif", "social-preview.png",
                     "hero-dark.png", "grid-diff-dark.png", "history-dark.png", "history.mp4"]:
            self.assertIn(name, self.assets(run_dir))
        # The README clip stops at the end of the tour, not after the follow-ups:
        # 8 s at 2x, 8 s, 3 s at 4x, 14 s.
        self.assertAlmostEqual(duration(os.path.join(run_dir, "assets", "demo.mp4")), 4 + 8 + 0.75 + 14, delta=0.3)

    def test_fleet(self):
        m = {"start": 0.5, "app-ready": 1, "agents-ready": 3, "prompt-sent": 5, "first-opened": 12,
             "all-opened": 25, "waiting": 40, "fleet-hold": 44, "end": 52}
        mov, run_dir = synthetic_run(self.tmp, "fleet", "light", m, 55)
        process(mov, run_dir)
        self.assertEqual(self.assets(run_dir), ["fleet-light.png", "fleet.mp4", "marks.json", "window.mp4"])
        # 2 s, then 20 s at 2x, 15 s at 3x, then up to 1.5 s before the end.
        self.assertAlmostEqual(duration(os.path.join(run_dir, "assets", "fleet.mp4")),
                               2 + 20 / 2 + 15 / 3 + (52 - 1.5 - 40), delta=0.3)

    def test_host_paints_the_gap(self):
        m = {"start": 0.5, "app-ready": 1, "agents-ready": 3, "prompts-sent": 5, "close": 15,
             "closed": 16.5, "reopen": 20, "reopened": 24, "sessions-back": 25, "end": 35}
        gap = (15.6, 22.2)
        mov, run_dir = synthetic_run(self.tmp, "host", "dark", m, 37, gap=gap)
        out = process(mov, run_dir).stdout
        self.assertIn("no window from", out)
        clip = os.path.join(run_dir, "assets", "host.mp4")
        lead = (15 - 5) / 2
        self.assertAlmostEqual(duration(clip), lead + (35 - 1.5 - 15), delta=0.3)
        # In the middle of the gap the clip is the window's colour, not the
        # test pattern behind it; before and after, the window is there.
        mid = lead + (sum(gap) / 2 - 15)
        r, g, b = frame_rgb(clip, mid)
        self.assertTrue(abs(r - 0x1E) < 12 and abs(g - 0x22) < 12 and abs(b - 0x27) < 12, (r, g, b))
        # Without the fill, the pattern shows through.
        shutil.rmtree(os.path.join(run_dir, "assets"))
        process(mov, run_dir, env={"HOST_FILL": "0"})
        r2, g2, b2 = frame_rgb(clip, mid)
        self.assertGreater(abs(r2 - r) + abs(g2 - g) + abs(b2 - b), 30)

    def test_missing_mark_is_refused(self):
        m = {"start": 0.5, "agents-ready": 1, "prompt-sent": 2, "end": 8}
        mov, run_dir = synthetic_run(self.tmp, "fleet", "dark", m, 9)
        result = subprocess.run([os.path.join(HERE, "process.sh"), mov, run_dir], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no mark all-opened", result.stderr)


class HostPids(unittest.TestCase):
    def test_only_the_stage_host_is_picked(self):
        # Short, as the stage is: a socket path has to fit in sun_path.
        tmp = tempfile.mkdtemp(prefix="sl-h.", dir="/tmp")
        self.addCleanup(shutil.rmtree, tmp, True)
        ours, theirs = os.path.join(tmp, "a"), os.path.join(tmp, "b")
        # Stand-ins for two hosts: the same command line, each listening on
        # the host socket of its own stage.
        # (nc, not Python: macOS's Python re-executes itself and loses the
        # command line it was given.)
        procs = []
        for stage in (ours, theirs):
            sock = os.path.join(stage, "tmp", "splitlane-dev", "splitlane-dev-host.sock")
            os.makedirs(os.path.dirname(sock))
            procs.append(subprocess.Popen(
                ["bash", "-c", 'exec -a "splitlane __splitlane-host" nc -lU "$1"', "-", sock],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL))
        def stop():
            for p in procs:
                p.kill()
                p.wait()
        self.addCleanup(stop)
        for _ in range(50):
            if all(os.path.exists(os.path.join(s, "tmp", "splitlane-dev", "splitlane-dev-host.sock")) for s in (ours, theirs)):
                break
            time.sleep(0.1)
        lib = f'source "{HERE}/lib.sh"'
        found = run(["bash", "-c", lib + '; stage_host_pids "$1"', "-", ours]).stdout.split()
        self.assertEqual(found, [str(procs[0].pid)])
        # Nothing at all for a stage with no host.
        self.assertEqual(run(["bash", "-c", lib + '; stage_host_pids "$1"', "-", tmp]).stdout, "")
        run(["bash", "-c", lib + '; stop_stage_hosts "$1"', "-", ours])
        self.assertIsNotNone(procs[0].wait(timeout=5))
        self.assertIsNone(procs[1].poll(), "the host of another stage was stopped")

    def test_processes_started_from_the_stage_are_stopped(self):
        tmp = tempfile.mkdtemp(prefix="sl-p.", dir="/tmp")
        self.addCleanup(shutil.rmtree, tmp, True)
        procs = []
        for stage in ("a", "ab"):  # "ab" starts with "a": the match is on "<stage>/"
            bin_dir = os.path.join(tmp, stage, "home", ".codex", "bin")
            os.makedirs(bin_dir)
            # A script, not a copy of a system binary: macOS kills a copied
            # platform binary on launch.
            with open(os.path.join(bin_dir, "codex"), "w") as f:
                f.write("#!/bin/bash\nwhile :; do sleep 0.2; done\n")
            os.chmod(os.path.join(bin_dir, "codex"), 0o755)
            procs.append(subprocess.Popen([os.path.join(bin_dir, "codex"), "app-server"]))
        def stop():
            for p in procs:
                p.kill()
                p.wait()
        self.addCleanup(stop)
        time.sleep(0.3)
        run(["bash", "-c", f'source "{HERE}/lib.sh"; stop_stage_processes "$1"', "-", os.path.join(tmp, "a")])
        self.assertIsNotNone(procs[0].wait(timeout=5))
        self.assertIsNone(procs[1].poll(), "a process of another stage was stopped")


if __name__ == "__main__":
    unittest.main()
