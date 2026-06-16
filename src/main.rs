mod encode;
mod log;
mod render;
mod resample;

use anyhow::{Context, Result, bail};
use clap::Parser;
use encode::{Codec, Encoder};
use log::Flight;
use rayon::prelude::*;
use render::{Norm, Renderer, Style};
use std::path::{Path, PathBuf};

/// Render a transparent control-stick overlay video from Betaflight blackbox logs.
#[derive(Parser, Debug)]
#[command(name = "stickoverlay", version, about, long_about = None)]
struct Args {
    /// Input .bbl/.BBL files and/or directories (directories are scanned for logs).
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// List logs (duration, start, field ranges) without rendering.
    #[arg(long)]
    info: bool,

    /// Output frames per second.
    #[arg(long, default_value_t = 50.0)]
    fps: f64,

    /// Radio mode (1-4) determining stick assignments. Default Mode 2.
    #[arg(long, default_value_t = 2)]
    mode: u8,

    /// Output directory (defaults to alongside each input).
    #[arg(long)]
    out: Option<PathBuf>,

    /// Codec: qtrle (default, lossless RGBA), prores4444, or webm (vp9).
    #[arg(long, default_value = "qtrle")]
    codec: String,

    /// Enable the phosphor-decay motion trail.
    #[arg(long)]
    trail: bool,

    /// Trail decay per frame (0..1, higher = longer smear). Default 0.88.
    #[arg(long, default_value_t = 0.88)]
    trail_decay: f32,

    /// Max trail opacity, so it reads as faint (0..1). Default 0.5.
    #[arg(long, default_value_t = 0.5)]
    trail_alpha: f32,

    /// Trail color as RRGGBB[AA] hex (default white).
    #[arg(long)]
    trail_color: Option<String>,

    /// Canvas size as WxH (default derived from box/gap/padding).
    #[arg(long)]
    size: Option<String>,

    /// Multiply all geometry (box/gap/pad/line/dot/corner) and canvas by this
    /// factor. Render at the size the overlay occupies on screen so thin lines
    /// survive (e.g. shrinking to 15% on 4K ≈ raise line-width/dot-radius ~6.7x).
    #[arg(long, default_value_t = 1.0)]
    render_scale: f32,

    #[arg(long)]
    box_size: Option<f32>,
    #[arg(long)]
    gap: Option<f32>,
    #[arg(long)]
    padding: Option<f32>,
    #[arg(long)]
    dot_radius: Option<f32>,
    #[arg(long)]
    line_width: Option<f32>,
    /// Box corner radius in px (0 = square corners). Default 14.
    #[arg(long)]
    corner_radius: Option<f32>,

    /// Colors as RRGGBB or RRGGBBAA hex.
    #[arg(long)]
    color_border: Option<String>,
    #[arg(long)]
    color_cross: Option<String>,
    #[arg(long)]
    color_dot: Option<String>,
    /// Box interior fill (default semi-transparent black).
    #[arg(long)]
    color_fill: Option<String>,

    /// Half-range for roll/pitch/yaw normalization (default 500).
    #[arg(long, default_value_t = 500.0)]
    rp_range: f32,
    /// Throttle low value mapping to box bottom (default 1000).
    #[arg(long, default_value_t = 1000.0)]
    throttle_min: f32,
    /// Throttle high value mapping to box top (default 2000).
    #[arg(long, default_value_t = 2000.0)]
    throttle_max: f32,

    #[arg(long)]
    invert_roll: bool,
    #[arg(long)]
    invert_pitch: bool,
    #[arg(long)]
    invert_yaw: bool,
    #[arg(long)]
    invert_throttle: bool,

    /// Dump N evenly-spaced PNG frames per log instead of encoding video.
    #[arg(long, default_value_t = 0)]
    debug_frames: usize,

    /// Worker threads for parallel batch (0 = rayon default = num CPUs).
    #[arg(long, default_value_t = 0)]
    threads: usize,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.threads > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(args.threads)
            .build_global()
            .ok();
    }

    let files = collect_inputs(&args.inputs)?;
    if files.is_empty() {
        bail!("no .bbl files found in the given inputs");
    }

    let codec = Codec::parse(&args.codec)?;
    let style = build_style(&args)?;
    let norm = build_norm(&args);
    let map = render::mapping(args.mode)?;

    if args.info {
        for file in &files {
            print_info(file);
        }
        return Ok(());
    }

    // Build the full work list (file, flight) so logs render in parallel even
    // across files.
    let mut jobs: Vec<(PathBuf, Flight, usize)> = Vec::new();
    for file in &files {
        match read_flights(file) {
            Ok(flights) => {
                let multi = flights.len() > 1;
                for f in flights {
                    jobs.push((file.clone(), f, if multi { 1 } else { 0 }));
                }
            }
            Err(e) => eprintln!("{}: {e:#}", file.display()),
        }
    }

    let results: Vec<Result<()>> = jobs
        .par_iter()
        .map(|(file, flight, suffix_mode)| {
            render_flight(args.out.as_deref(), file, flight, *suffix_mode, &style, &norm, map, codec, args.fps, args.debug_frames)
                .with_context(|| format!("{} log {}", file.display(), flight.index))
        })
        .collect();

    let mut failed = 0;
    for r in results {
        if let Err(e) = r {
            failed += 1;
            eprintln!("error: {e:#}");
        }
    }
    if failed > 0 {
        bail!("{failed} log(s) failed");
    }
    Ok(())
}

fn build_style(args: &Args) -> Result<Style> {
    let mut s = Style::default();
    if let Some(b) = args.box_size {
        s.box_size = b;
    }
    if let Some(g) = args.gap {
        s.gap = g;
    }
    if let Some(p) = args.padding {
        s.pad = p;
    }
    if let Some(r) = args.dot_radius {
        s.dot_radius = r;
    }
    if let Some(w) = args.line_width {
        s.line_width = w;
    }
    if let Some(r) = args.corner_radius {
        s.corner_radius = r;
    }
    // Uniform render scale (for HiDPI / rendering at on-screen size).
    let sc = args.render_scale;
    if sc > 0.0 && sc != 1.0 {
        s.box_size *= sc;
        s.gap *= sc;
        s.pad *= sc;
        s.dot_radius *= sc;
        s.line_width *= sc;
        s.corner_radius *= sc;
    }
    // Recompute default canvas from (possibly overridden) geometry.
    s.canvas_w = (s.pad * 2.0 + s.box_size * 2.0 + s.gap).round() as u32;
    s.canvas_h = (s.pad * 2.0 + s.box_size).round() as u32;
    if let Some(size) = &args.size {
        let (w, h) = parse_size(size)?;
        s.canvas_w = w;
        s.canvas_h = h;
    }
    if let Some(c) = &args.color_border {
        s.col_border = render::parse_color(c)?;
    }
    if let Some(c) = &args.color_cross {
        s.col_cross = render::parse_color(c)?;
    }
    if let Some(c) = &args.color_dot {
        s.col_dot = render::parse_color(c)?;
    }
    if let Some(c) = &args.color_fill {
        s.col_fill = render::parse_color(c)?;
    }
    if let Some(c) = &args.trail_color {
        s.col_trail = render::parse_color(c)?;
    }
    s.trail = args.trail;
    s.trail_decay = args.trail_decay.clamp(0.0, 1.0);
    s.trail_alpha = args.trail_alpha.clamp(0.0, 1.0);
    Ok(s)
}

fn build_norm(args: &Args) -> Norm {
    Norm {
        rp_half: args.rp_range,
        thr_min: args.throttle_min,
        thr_max: args.throttle_max,
        inv_roll: args.invert_roll,
        inv_pitch: args.invert_pitch,
        inv_yaw: args.invert_yaw,
        inv_throttle: args.invert_throttle,
    }
}

fn parse_size(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .context("--size must be WxH, e.g. 880x480")?;
    Ok((w.trim().parse()?, h.trim().parse()?))
}

/// Expand inputs into a deduplicated list of .bbl files.
fn collect_inputs(inputs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let push = |p: PathBuf, out: &mut Vec<PathBuf>| {
        if !out.contains(&p) {
            out.push(p);
        }
    };
    for input in inputs {
        if input.is_dir() {
            for pat in ["*.bbl", "*.BBL", "*.bfl", "*.BFL"] {
                let g = input.join(pat);
                for entry in glob::glob(&g.to_string_lossy())? {
                    if let Ok(p) = entry {
                        push(p, &mut out);
                    }
                }
            }
        } else if input.exists() {
            push(input.clone(), &mut out);
        } else {
            // Treat as a glob pattern.
            let mut matched = false;
            for entry in glob::glob(&input.to_string_lossy())? {
                if let Ok(p) = entry {
                    matched = true;
                    push(p, &mut out);
                }
            }
            if !matched {
                eprintln!("warning: no match for input '{}'", input.display());
            }
        }
    }
    out.sort();
    Ok(out)
}

fn read_flights(file: &Path) -> Result<Vec<Flight>> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    log::parse_file(&bytes)
}

fn print_info(file: &Path) {
    println!("{}", file.display());
    let flights = match read_flights(file) {
        Ok(f) => f,
        Err(e) => {
            println!("  error: {e:#}");
            return;
        }
    };
    for f in &flights {
        let r = f.ranges();
        println!(
            "  log {}: {} samples, {:.2}s, start={}us ({:.0} Hz)",
            f.index,
            f.samples.len(),
            f.duration_us() as f64 / 1e6,
            f.start_us(),
            if f.duration_us() > 0 {
                (f.samples.len() as f64 - 1.0) / (f.duration_us() as f64 / 1e6)
            } else {
                0.0
            }
        );
        for (n, (lo, hi)) in ["roll", "pitch", "yaw", "throttle"].iter().zip(r) {
            println!("      {n:9} min={lo:8.1} max={hi:8.1} range={:7.1}", hi - lo);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn render_flight(
    out_dir: Option<&Path>,
    file: &Path,
    flight: &Flight,
    suffix_mode: usize,
    style: &Style,
    norm: &Norm,
    map: render::Mapping,
    codec: Codec,
    fps: f64,
    debug_frames: usize,
) -> Result<()> {
    let stem = file.file_stem().unwrap_or_default().to_string_lossy();
    let suffix = if suffix_mode == 1 {
        format!("_{}", flight.index)
    } else {
        String::new()
    };
    let dir = out_dir
        .map(Path::to_path_buf)
        .or_else(|| file.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).ok();

    let mut renderer = Renderer::new(style.clone(), *norm, map)?;
    let samples = &flight.samples;

    if debug_frames > 0 {
        for k in 0..debug_frames {
            let frac = if debug_frames == 1 {
                0.5
            } else {
                k as f64 / (debug_frames - 1) as f64
            };
            let idx = ((samples.len() - 1) as f64 * frac).round() as usize;
            renderer.render(&samples[idx]);
            let png = dir.join(format!("{stem}{suffix}_debug{k}.png"));
            renderer.save_png(&png)?;
        }
        println!(
            "{}{}: {} debug PNG(s) -> {}",
            stem,
            suffix,
            debug_frames,
            dir.display()
        );
        return Ok(());
    }

    let n_frames = resample::frame_count(flight.duration_us(), fps).max(1);
    let out_path = dir.join(format!("{stem}{suffix}.{}", codec.ext()));
    let mut enc = Encoder::new(&out_path, renderer.width(), renderer.height(), fps, codec)?;

    let start = flight.start_us() as f64;
    let mut cursor = 0usize;
    for i in 0..n_frames {
        let t_us = start + (i as f64) / fps * 1e6;
        let s = resample::interp_at(samples, &mut cursor, t_us);
        let frame = renderer.render(&s);
        enc.write_frame(frame)?;
    }
    enc.finish()?;

    println!(
        "{} -> {} frames @ {} fps -> {}",
        flight.index,
        n_frames,
        fps,
        out_path.display()
    );
    Ok(())
}
