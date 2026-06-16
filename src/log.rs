//! Blackbox log parsing: extract per-frame timestamp + rcCommand[0..3].

use anyhow::{Context, Result, bail};
use blackbox_log::frame::{Frame, FieldDef};
use blackbox_log::prelude::*;

/// One decoded RC sample from a log frame.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
    /// Microseconds since power-on (raw blackbox time counter).
    pub t_us: u64,
    pub roll: f32,     // rcCommand[0]
    pub pitch: f32,    // rcCommand[1]
    pub yaw: f32,      // rcCommand[2]
    pub throttle: f32, // rcCommand[3]
}

/// One armed flight (a single log inside a .bbl, which may hold several).
pub struct Flight {
    /// 1-based index within the source file.
    pub index: usize,
    pub samples: Vec<Sample>,
}

impl Flight {
    pub fn duration_us(&self) -> u64 {
        match (self.samples.first(), self.samples.last()) {
            (Some(a), Some(b)) => b.t_us.saturating_sub(a.t_us),
            _ => 0,
        }
    }

    pub fn start_us(&self) -> u64 {
        self.samples.first().map(|s| s.t_us).unwrap_or(0)
    }

    /// min/max for each channel: (roll, pitch, yaw, throttle).
    pub fn ranges(&self) -> [(f32, f32); 4] {
        let mut r = [(f32::INFINITY, f32::NEG_INFINITY); 4];
        for s in &self.samples {
            for (i, v) in [s.roll, s.pitch, s.yaw, s.throttle].into_iter().enumerate() {
                if v < r[i].0 {
                    r[i].0 = v;
                }
                if v > r[i].1 {
                    r[i].1 = v;
                }
            }
        }
        r
    }
}

fn value_to_f32(v: blackbox_log::frame::MainValue) -> f32 {
    use blackbox_log::frame::MainValue;
    match v {
        MainValue::Signed(x) => x as f32,
        MainValue::Unsigned(x) => x as f32,
        _ => f32::NAN,
    }
}

/// blackbox-log 0.4.3 rejects Betaflight's date-based versioning (e.g.
/// "2025.12.2"): the version components overflow `u8` and fall outside the
/// supported 4.2..4.6 range. The numeric frame fields we read (time, rcCommand)
/// are decoded from the self-describing headers, not from the firmware enum, so
/// we rewrite the firmware-revision line to a supported version before parsing.
/// Returns the rewritten bytes and the original firmware string if changed.
///
/// Native support is coming upstream in blackbox-log PR #168
/// (<https://github.com/blackbox-log/blackbox-log/pull/168>); once that is
/// released this shim can be dropped and the dependency bumped. Verified that
/// decoding a real 2025.12.2 log via this shim yields rcCommand values identical
/// to PR #168's native parse.
fn normalize_firmware(bytes: &[u8]) -> (Vec<u8>, Option<String>) {
    const NEEDLE: &[u8] = b"H Firmware revision:";
    let mut out = Vec::with_capacity(bytes.len());
    let mut original = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(NEEDLE) {
            out.extend_from_slice(NEEDLE);
            let start = i + NEEDLE.len();
            let end = bytes[start..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| start + p)
                .unwrap_or(bytes.len());
            let value = &bytes[start..end];
            if let (Some(rewritten), Some(orig)) = rewrite_fw_value(value) {
                original = Some(orig);
                out.extend_from_slice(rewritten.as_bytes());
            } else {
                out.extend_from_slice(value);
            }
            i = end;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    (out, original)
}

/// Given a firmware-revision value like "Betaflight 2025.12.2 (hash) BOARD",
/// returns (rewritten_value, original_value) if the version is unsupported and
/// was clamped to a supported one, else (None, None).
fn rewrite_fw_value(value: &[u8]) -> (Option<String>, Option<String>) {
    let s = match std::str::from_utf8(value) {
        Ok(s) => s,
        Err(_) => return (None, None),
    };
    let mut tokens: Vec<&str> = s.split(' ').collect();
    if tokens.len() < 2 {
        return (None, None);
    }
    let kind = tokens[0].to_ascii_lowercase();
    let ver = tokens[1];
    let major = ver.split('.').next().and_then(|m| m.parse::<u32>().ok());
    let replacement = match (kind.as_str(), major) {
        // Already-supported Betaflight 4.x / INAV 5-8 parse fine; leave alone.
        ("betaflight", Some(4)) => None,
        ("inav", Some(5..=8)) => None,
        ("betaflight", _) => Some("4.5.0"),
        ("inav", _) => Some("8.0.0"),
        _ => None,
    };
    match replacement {
        Some(rep) => {
            let orig = s.to_string();
            tokens[1] = rep;
            (Some(tokens.join(" ")), Some(orig))
        }
        None => (None, None),
    }
}

/// Parse every log in a .bbl file. Errors per-log are surfaced but do not abort
/// the others.
pub fn parse_file(bytes: &[u8]) -> Result<Vec<Flight>> {
    let (bytes, original_fw) = normalize_firmware(bytes);
    if let Some(orig) = original_fw {
        eprintln!("  note: '{orig}' not natively supported — decoding as Betaflight 4.5");
    }
    let file = blackbox_log::File::new(&bytes);
    let mut flights = Vec::new();

    for (i, headers) in file.iter().enumerate() {
        let index = i + 1;
        let headers = match headers {
            Ok(h) => h,
            Err(e) => {
                eprintln!("  log {index}: skipping — failed to parse headers: {e:?}");
                continue;
            }
        };

        match parse_one(&headers) {
            Ok(samples) if samples.is_empty() => {
                eprintln!("  log {index}: skipping — no decoded frames");
            }
            Ok(samples) => flights.push(Flight { index, samples }),
            Err(e) => eprintln!("  log {index}: skipping — {e:#}"),
        }
    }

    if flights.is_empty() {
        bail!("no usable logs found in file");
    }
    Ok(flights)
}

fn parse_one(headers: &Headers) -> Result<Vec<Sample>> {
    let def = headers.main_frame_def();

    // Locate rcCommand[0..3] column indices by field name.
    let mut idx: [Option<usize>; 4] = [None; 4];
    for (j, field) in def.iter().enumerate() {
        let FieldDef { name, .. } = field;
        match name {
            "rcCommand[0]" => idx[0] = Some(j),
            "rcCommand[1]" => idx[1] = Some(j),
            "rcCommand[2]" => idx[2] = Some(j),
            "rcCommand[3]" => idx[3] = Some(j),
            _ => {}
        }
    }

    let idx = {
        let mut out = [0usize; 4];
        for (k, slot) in idx.iter().enumerate() {
            out[k] = slot.with_context(|| {
                format!("log is missing rcCommand[{k}] — was it logged with RC Commands enabled?")
            })?;
        }
        out
    };

    let mut samples = Vec::new();
    let mut parser = headers.data_parser();
    while let Some(event) = parser.next() {
        if let ParserEvent::Main(frame) = event {
            let get = |c: usize| frame.get(idx[c]).map(value_to_f32).unwrap_or(f32::NAN);
            samples.push(Sample {
                t_us: frame.time_raw(),
                roll: get(0),
                pitch: get(1),
                yaw: get(2),
                throttle: get(3),
            });
        }
    }

    Ok(samples)
}
