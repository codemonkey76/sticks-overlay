Build a Rust CLI tool that renders a transparent "stick input" overlay video from
Betaflight blackbox logs, for compositing over FPV flight footage in a video editor.

## Context

I fly FPV quads and want a clean control-stick overlay (the two gimbal boxes with
moving dots) on my flight videos. My blackbox logs are stripped down to ONLY the
RC Commands field, logged at 125 Hz (Betaflight 4.5, 2 kHz loop, blackbox rate 1/16).
My footage is 50 fps. The overlay will be dropped on a track above the footage in
Kdenlive, so it MUST have a real alpha channel (transparent background). I'm on Linux;
ffmpeg is installed. Existing tools (BlackboxSticksExporter) are Windows-only, which is
why I'm building this.

## Goal

A single static binary, `stickoverlay`, that takes one or more .bbl files (or a
directory) and produces one transparent .mov per flight log, batched.

## Parsing

- Use the `blackbox-log` crate (v0.4.3) to parse logs natively — do NOT shell out to
  the C `blackbox_decode`. Check its current API on docs.rs before coding; fall back to
  `fc-blackbox` only if blackbox-log can't expose what's needed.
- A single .bbl can contain MULTIPLE logs (one per arm). Handle that: render each log,
  suffixing output `_1`, `_2`, etc.
- Per frame, extract: timestamp (microseconds) and rcCommand[0..3].
- Field meanings: rcCommand[0]=roll, [1]=pitch, [2]=yaw, [3]=throttle.
- IMPORTANT: before hardcoding any normalization, add a step (or `--info` mode) that
  prints each field's actual min/max/range from a real log. Roll/pitch/yaw are roughly
  -500..+500 centered on 0; throttle is roughly 1000..2000 (NOT centered) — but VERIFY
  empirically against my logs rather than trusting these numbers.

## Stick mapping (Mode 2 default, make `--mode` 1-4 configurable)

- Left box: X = yaw (rcCommand[2]), Y = throttle (rcCommand[3])
- Right box: X = roll (rcCommand[0]), Y = pitch (rcCommand[1])
- Normalize roll/pitch/yaw to -1..1 (clamp); throttle to 0..1 (clamp).
- Pitch/throttle vertical direction sign is easy to get backwards — expose
  `--invert-pitch` / axis-flip flags and pick sensible defaults (throttle: up = high).

## Resampling

- Output at a configurable `--fps` (default 50). Logs are higher-rate and NOT evenly
  spaced, so for each output frame at t = t_start + i/fps, find the bracketing log
  samples and LINEARLY INTERPOLATE rcCommand. Use the log's real microsecond timestamps.
- Output duration = log duration; frame count = round(duration \* fps).

## Rendering

- Draw to an RGBA buffer, fully transparent background (alpha 0 except drawn pixels).
- Two boxes side by side: border, center crosshair, and a filled dot at the current
  stick position. Optional fading trail of recent positions (`--trail N`).
- Prefer `tiny-skia` for anti-aliased output; `image`+`imageproc` is an acceptable
  fallback. Anti-aliasing matters — it'll be scaled in the editor.
- Configurable: canvas size, box size, gap, padding, colors, dot radius, line width.

## Encoding (the key part)

- Stream raw RGBA frames to ffmpeg via stdin; don't write a PNG sequence and don't hold
  all frames in memory. Spawn ffmpeg as a child process and write to its stdin:
  ffmpeg -f rawvideo -pixel_format rgba -video_size WxH -framerate FPS -i -
  -c:v qtrle -y OUT.mov
- qtrle (QuickTime Animation) is lossless with alpha and works in Kdenlive/MLT.
- Support `--codec` with alternates: prores4444 (-c:v prores_ks -profile:v 4444
  -pix_fmt yuva444p10le) and transparent webm (vp9, yuva420p).

## CLI

- `clap` for args. Accept multiple paths and directories (glob _.bbl/_.BBL).
- Flags: --fps, --mode, --size, --out (dir), --codec, --trail, --invert-pitch,
  throttle/range overrides, --threads.
- `--info`: list logs in each file with duration, start timestamp, and field ranges,
  WITHOUT rendering (I use this to match which flight goes with which video clip).
- Batch logs in parallel with `rayon` (logs are independent).

## Testing / acceptance

- Fetch a sample Betaflight .bbl (from the blackbox-log crate's test fixtures or
  betaflight/blackbox-tools) and validate end to end.
- Add `--debug-frames N` to dump a few PNGs for visual inspection: at idle the dots sit
  centered (throttle dot at bottom), and they track input.
- Verify the output: `ffprobe` shows an alpha-carrying codec, and frame count ≈
  duration × fps.
- Errors must be clear if a log lacks rcCommand, or is empty/corrupt.

## Deliverables

- Working Cargo project, good `--help`.
- README with usage, the exact mapping/normalization assumptions you settled on, and a
  short "Kdenlive workflow" section: place overlay on the track above the footage, slide
  it to align with the arm-time motor-spool spike, group both clips, trim the ends,
  render H.264 at the footage's fps.

## Approach

Start with a thin vertical slice: parse one log, print field ranges, render a SINGLE
PNG frame at a chosen timestamp, and eyeball that the dots are placed correctly. Only
then wire up resampling, the ffmpeg pipe, batching, and parallelism. Confirm the mapping
visually before optimizing anything.
