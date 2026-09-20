# Alacritty backend baseline

Captured on 2026-07-14 on Linux x86_64, Cargo `release` profile, deterministic
seed `0x50414e45464c4f57`. It is the reference the libghostty backend is
measured against.

```sh
cargo test -p splitlane-app terminal::backend_corpus::alacritty_eight_pane_baseline --release --locked --no-default-features -- --ignored --nocapture
```

| Measure | Value |
|---|---:|
| Persistent panes | 8 |
| Streams per pane | 100 |
| Total input | 62,400 bytes |
| Parser throughput | 2.964 MiB/s |
| Input to snapshot p50 | 8 µs |
| Input to snapshot p95 | 107 µs |
| Lock p95 | 17 µs |
| Wall time | 20 ms |
| CPU time | 10 ms |
| Starting RSS | 8,286,208 bytes |
| Final RSS | 22,740,992 bytes |
| CPU | AMD Ryzen 7 7800X3D 8-Core Processor |

Raw output:

```json
{"seed":"0x50414e45464c4f57","panes":8,"streams_per_pane":100,"bytes":62400,"throughput_mib_s":2.964,"input_to_snapshot_p50_us":8,"input_to_snapshot_p95_us":107,"lock_p95_us":17,"wall_ms":20,"cpu_ms":10,"rss_start_bytes":8286208,"rss_end_bytes":22740992,"cpu_model":"AMD Ryzen 7 7800X3D 8-Core Processor","profile":"release","measurement_scope":"persistent-eight-pane-parser-to-neutral-snapshot"}
```

This baseline measures the path from the Alacritty parser to a neutral
snapshot with eight persistent terminals. It does not measure the time until
a GPUI frame is presented, so on its own it does not answer an input-to-frame
question.

## GPUI end-to-end scenario

Recorded the same day. The test that produced it,
`layout::render::tests::alacritty_eight_pane_gpui_input_to_paint_baseline`,
is no longer in the tree: it laid out eight panes, and a container now holds
at most four. The numbers are kept as a historical reference.

The scenario kept eight `Pane`s active in a production `LayoutTree` of two
rows by four columns. Each sample injected the same synthetic stream into all
eight `TerminalView`s, invalidated the entities, then waited for the GPUI
dispatcher to park. The clock therefore stopped after the scene was built,
once all eight `TerminalElement::paint` calls had finished.

| Measure | Value |
|---|---:|
| Active panes | 8 |
| Streams per pane | 100 |
| Total input | 62,400 bytes |
| Injection-to-paint throughput | 0.047 MiB/s |
| Input-to-frame p50 | 11,962 µs |
| Input-to-frame p95 | 15,902 µs |
| Measured `render_content` acquisitions | 6,720 |
| `render_content` lock hold p50 | 7 µs |
| `render_content` lock hold p95 | 7 µs |
| Wall time | 1,257 ms |
| CPU time | 1,250 ms |
| Starting RSS | 17,682,432 bytes |
| Peak RSS | 26,447,872 bytes |
| Final RSS | 26,447,872 bytes |
| CPU | AMD Ryzen 7 7800X3D 8-Core Processor |
| Platform | Linux x86_64 |
| Profile | Cargo `release` |
| Seed | `0x50414e45464c4f57` |

Raw output:

```json
{"seed":"0x50414e45464c4f57","panes":8,"streams_per_pane":100,"bytes":62400,"throughput_mib_s":0.047,"input_to_frame_p50_us":11962,"input_to_frame_p95_us":15902,"render_content_lock_samples":6720,"render_content_lock_held_p50_us":7,"render_content_lock_held_p95_us":7,"wall_ms":1257,"cpu_ms":1250,"rss_start_bytes":17682432,"rss_peak_bytes":26447872,"rss_end_bytes":26447872,"hardware":"AMD Ryzen 7 7800X3D 8-Core Processor","platform":"linux-x86_64","profile":"release","measurement_boundary":"byte injection through GPUI dispatcher parked after TerminalElement::paint","lock_measurement":"all render_content terminal-lock hold durations from the measured GPUI paints","presentation_scope":"GPUI test-platform scene generation; excludes Window::present, GPU submission, compositor, and display scanout"}
```

This measurement covers the whole boundary: byte injection, Alacritty
parsing, the neutral snapshot, GPUI layout and the end of the terminal paint.
The probe started after the warm-up paint and captured the 6,720 lock hold
durations produced by the real `render_content` passes during the 100
measured cycles. GPUI ran several complete eight-pane passes before parking
its dispatcher; no artificial snapshot loop was added after the frames. It
does not cover `Window::present`, GPU submission, the Wayland/X11 compositor
or display scanout. The earlier parser-to-snapshot metric stays separate and
keeps its name, `input_to_snapshot`.
