# stickoverlay — Specification

A Rust CLI that renders a transparent control-stick overlay video from Betaflight
blackbox logs, for compositing over FPV flight footage in a video editor.

## Background & motivation

I fly FPV quads and want a clean control-stick overlay — the two gimbal boxes with
moving dots — on my flight videos. My blackbox logs are deliberately stripped to **only
the RC Commands field**, logged at **125 Hz** (Betaflight 4.5, 2 kHz loop, blackbox rate
1/16). Footage is **50 fps**. The overlay goes on a track above the footage in
**Kdenlive**, so it **must have a real alpha channel** (transparent background).

Environment: **Linux**, `ffmpeg` installed, Rust toolchain available. The existing tool
(BlackboxSticksExporter) is Windows-only, which is why this exists.

## Goal

A single static binary, `stickoverlay`, that takes one or more `.bbl` files (or a
directory) and produces **one transparent `.mov` per flight log**, batched.

## Non-goals

- No GUI.
- No flight-data analysis (PID/gyro/tuning) — logs only contain RC Commands anyway.
- No video compositing/sync — that happens in Kdenlive. This tool only emits the
  transparent overlay.
- Not a general blackbox dashboard renderer; sticks only.

---

## Parsing

- Use the **`blackbox-log` crate (v0.4.3)** to parse logs natively. **Do not** shell out
  to the C `blackbox_decode`. Check the crate's current API on docs.rs before coding.
  Fall back to `fc-blackbox` only if `blackbox-log` can't expose what's needed.
- A single `.bbl` can contain **multiple logs** (one per arm). Render each, suffixing the
  output `_1`, `_2`, etc.
- Per frame extract: **timestamp (microseconds)** and **`rcCommand[0..3]`**.
- Field meanings: `rcCommand[0]=roll`, `[1]=pitch`, `[2]=yaw`, `[3]=throttle`.
- **Verify ranges empirically.** Before hardcoding normalization, print each field's
  actual min/max/range from a real log (see `--info`). Rough expectations to sanity-check
  against, NOT to trust blindly: roll/pitch/yaw ≈ −500..+500 centered on 0; throttle ≈
  1000..2000 (not centered).

## Stick mapping

Mode 2 is the default; make `--mode` (1–4) configurable.

| Box   | X axis              | Y axis                  |
| ----- | ------------------- | ----------------------- |
| Left  | yaw `rcCommand[2]`  | throttle `rcCommand[3]` |
| Right | roll `rcCommand[0]` | pitch `rcCommand[1]`    |

- Normalize roll/pitch/yaw to −1..1 (clamp); throttle to 0..1 (clamp).
- Vertical direction sign is easy to get backwards. Default: **throttle up = high**,
  pitch up = forward/up. Expose `--invert-pitch` and per-axis flip flags.

## Resampling

- Output at `--fps` (default **50**).
- Log samples are higher-rate and **not evenly spaced**. For each output frame at
  `t = t_start + i/fps`, find the bracketing log samples and **linearly interpolate**
  `rcCommand` using the real microsecond timestamps.
- Output duration = log duration; frame count = `round(duration * fps)`.

## Rendering

- Draw to an **RGBA** buffer; background fully transparent (alpha 0 except drawn pixels).
- Two boxes side by side: rounded border, semi-transparent fill, center crosshair,
  filled dot at current stick position.
- Optional **phosphor-decay trail** (`--trail`): a persistent layer faded per frame
  by `--trail-decay`, so the smear stretches on fast moves and stays tight when held.
- Prefer **`tiny-skia`** for anti-aliased output; `image` + `imageproc` is an acceptable
  fallback. Anti-aliasing matters — the overlay gets scaled in the editor.
- Configurable: canvas size, box size, gap, padding, colors, dot radius, line width.

## Encoding (the key part)

- **Stream raw RGBA frames to ffmpeg via stdin.** Do not write a PNG sequence; do not
  buffer all frames in memory. Spawn ffmpeg as a child and write frames to its stdin:

  ```
  ffmpeg -f rawvideo -pixel_format rgba -video_size WxH -framerate FPS -i - \
         -c:v qtrle -y OUT.mov
  ```

- **qtrle** (QuickTime Animation) is lossless with alpha and works in Kdenlive/MLT —
  this is the default.
- `--codec` alternates:
  - prores4444: `-c:v prores_ks -profile:v 4444 -pix_fmt yuva444p10le`
  - transparent webm: vp9, `-pix_fmt yuva420p`

## CLI

- Use **`clap`**. Accept multiple paths and directories (glob `*.bbl` / `*.BBL`).
- Flags: `--fps`, `--mode`, `--size`, `--render-scale`, `--out <dir>`, `--codec`,
  `--trail` (+`--trail-decay`/`--trail-alpha`/`--trail-color`), geometry/color
  overrides (incl. `--corner-radius`, `--color-fill`), `--invert-pitch`,
  range/throttle overrides, `--threads`.
- `--info`: list logs in each file with duration, start timestamp, and field ranges,
  **without rendering**. Used to match which flight goes with which video clip.
- `--debug-frames N`: dump a few PNGs for visual inspection instead of/alongside video.
- Batch independent logs in parallel with **`rayon`**.

---

## Testing & acceptance

- Fetch a sample Betaflight `.bbl` (blackbox-log crate test fixtures, or
  betaflight/blackbox-tools) and validate end to end.
- `--debug-frames`: at idle the dots sit centered, with the throttle dot at the bottom;
  they track input correctly.
- Output check: `ffprobe` shows an alpha-carrying codec, and frame count ≈
  duration × fps.
- Clear errors when a log lacks `rcCommand`, or is empty/corrupt.

## Deliverables

- Working Cargo project with good `--help`.
- README covering usage, the **final mapping/normalization assumptions actually chosen**,
  and a short **Kdenlive workflow** section (below).

## Kdenlive workflow (for the README)

1. Export the overlay at the footage's fps (50).
2. Footage on track V1; the transparent overlay `.mov` on V2 above it — alpha composites
   automatically.
3. Slide the overlay to align with the arm-time motor-spool spike in the footage.
4. Select both clips, **Ctrl+G** to group so they stay in sync.
5. Trim the dead air off each end.
6. Render H.264/MP4 at the footage's resolution and fps; upload.

---

## Build approach (do this in order)

Start with a **thin vertical slice**, confirm visually, then expand:

1. Parse one log; print field ranges (`--info`).
2. Render a **single PNG** at a chosen timestamp; eyeball that dots are placed correctly.
3. Only then add resampling, the ffmpeg pipe, batching, and parallelism.
4. Confirm the mapping visually **before** optimizing anything.

## Known risk areas

- **Throttle/stick normalization** is the most likely thing to need tweaking — calibrate
  against a real log, not the rough range guesses above.
- **Axis sign** (throttle/pitch up-vs-down) — verify with a debug frame, not assumption.
- **Multiple logs per `.bbl`** — don't assume one log per file.
- **Uneven sample spacing** — interpolate on real timestamps; don't assume fixed dt.
