# stickoverlay

Render a transparent control-stick overlay video from Betaflight blackbox logs,
for compositing the two gimbal boxes (with moving dots) over FPV flight footage
in a video editor.

Parses `.bbl` logs natively (via the [`blackbox-log`](https://docs.rs/blackbox-log)
crate — no shelling out to `blackbox_decode`), resamples the RC commands onto an
even frame grid, draws anti-aliased boxes/dots with [`tiny-skia`](https://docs.rs/tiny-skia),
and streams raw RGBA frames to `ffmpeg` to produce an **alpha-carrying** video
that drops straight onto a track above your footage in Kdenlive.

## Requirements

- `ffmpeg` on `PATH` (tested with n8.x).
- Rust toolchain to build.

## Build

```sh
cargo build --release
# binary at target/release/stickoverlay
```

## Usage

```sh
# Inspect logs first — duration, start time, and ACTUAL field ranges.
# Use this to (a) match a flight to its video clip and (b) sanity-check the
# normalization ranges below against your radio/rates.
stickoverlay --info flight.bbl

# Render (Mode 2, 50 fps, lossless qtrle .mov by default).
stickoverlay flight.bbl

# A whole directory, in parallel, into one output folder.
stickoverlay --out overlays/ logs/

# Dump a few PNGs to eyeball stick placement without encoding video.
stickoverlay --debug-frames 5 flight.bbl
```

A single `.bbl` can contain several logs (one per arm). Each is rendered to its
own file; when there is more than one, outputs are suffixed `_1`, `_2`, ….

## Stick mapping & normalization assumptions

Per frame the tool reads `rcCommand[0..3]` = **roll, pitch, yaw, throttle**.

Default **Mode 2** (`--mode 1..4` to change):

| Box   | X axis        | Y axis            |
|-------|---------------|-------------------|
| Left  | yaw           | throttle (up=high)|
| Right | roll          | pitch (up=forward)|

Normalization (all clamped):

- **roll / pitch / yaw**: centered on 0, divided by `--rp-range` (default **500**)
  → `-1..1`.
- **throttle**: `(value - --throttle-min) / (--throttle-max - --throttle-min)`
  with defaults **1000 / 2000** → `0..1`, bottom-to-top.

These defaults match typical Betaflight `rcCommand` scaling, but **verify with
`--info`** — it prints each field's real min/max from your log. If throttle or
pitch moves the wrong way, use `--invert-throttle` / `--invert-pitch`
(also `--invert-roll`, `--invert-yaw`). If your ranges differ, override
`--rp-range` / `--throttle-min` / `--throttle-max`.

## Codecs (`--codec`)

| Value          | Container | Notes                                   |
|----------------|-----------|-----------------------------------------|
| `qtrle` (def)  | `.mov`    | QuickTime Animation, lossless RGBA. Safe in Kdenlive/MLT. |
| `prores4444`   | `.mov`    | ProRes 4444 + alpha, 10-bit.            |
| `webm`         | `.webm`   | VP9 with `yuva420p` alpha.              |

## Options

```
--fps <N>              output fps (default 50)
--mode <1-4>           radio mode (default 2)
--out <DIR>            output directory (default: alongside each input)
--codec <C>            qtrle | prores4444 | webm
--trail <N>            fading dot trail length (default 0)
--size <WxH>           explicit canvas size (default derived from geometry)
--box-size / --gap / --padding / --dot-radius / --line-width
--color-border / --color-cross / --color-dot / --color-trail  (RRGGBB[AA])
--rp-range / --throttle-min / --throttle-max
--invert-roll / --invert-pitch / --invert-yaw / --invert-throttle
--debug-frames <N>     dump N PNGs per log instead of encoding
--threads <N>          parallel workers (0 = all CPUs)
--info                 list logs + ranges, no rendering
```

## Kdenlive workflow

1. Render your overlay at the footage's fps (e.g. `--fps 50`).
2. Place the overlay clip on the **track above** your flight footage.
3. Slide it horizontally to align with the **arm-time motor-spool spike** — the
   moment throttle/stick activity begins lines up with the motors spooling in the
   video.
4. Select both clips and **Group** them so they stay locked while you trim.
5. Trim the ends to the usable flight.
6. Render the project to H.264 at the footage's fps.

The overlay has a true alpha channel, so it composites directly — no chroma key.

## Verifying output

```sh
ffprobe -v error -show_streams overlay.mov | grep -E 'codec_name|pix_fmt|nb_frames'
# frame count should be ≈ duration × fps
```
