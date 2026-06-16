//! Resample irregularly-spaced log samples onto an even output frame grid via
//! linear interpolation on real microsecond timestamps.

use crate::log::Sample;

pub fn frame_count(duration_us: u64, fps: f64) -> u64 {
    (duration_us as f64 / 1e6 * fps).round() as u64
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Interpolate the RC sample at `t_us`. `cursor` is advanced monotonically and
/// must start at 0 for the first (smallest) `t_us`.
pub fn interp_at(samples: &[Sample], cursor: &mut usize, t_us: f64) -> Sample {
    debug_assert!(!samples.is_empty());
    // Advance cursor so that samples[cursor].t_us <= t_us < samples[cursor+1].t_us.
    while *cursor + 1 < samples.len() && (samples[*cursor + 1].t_us as f64) <= t_us {
        *cursor += 1;
    }

    let a = &samples[*cursor];
    if *cursor + 1 >= samples.len() {
        return *a;
    }
    let b = &samples[*cursor + 1];
    let span = (b.t_us - a.t_us) as f64;
    let frac = if span > 0.0 {
        ((t_us - a.t_us as f64) / span).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };

    Sample {
        t_us: t_us as u64,
        roll: lerp(a.roll, b.roll, frac),
        pitch: lerp(a.pitch, b.pitch, frac),
        yaw: lerp(a.yaw, b.yaw, frac),
        throttle: lerp(a.throttle, b.throttle, frac),
    }
}
