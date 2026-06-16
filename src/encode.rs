//! Stream raw RGBA frames to ffmpeg's stdin to produce an alpha-carrying video.

use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

#[derive(Clone, Copy, Debug)]
pub enum Codec {
    /// QuickTime Animation — lossless RGBA, the safe default for Kdenlive/MLT.
    Qtrle,
    /// ProRes 4444 with alpha (10-bit).
    Prores4444,
    /// Transparent VP9 WebM.
    Webm,
}

impl Codec {
    pub fn parse(s: &str) -> Result<Codec> {
        Ok(match s.to_ascii_lowercase().as_str() {
            "qtrle" => Codec::Qtrle,
            "prores4444" | "prores" => Codec::Prores4444,
            "webm" | "vp9" => Codec::Webm,
            _ => bail!("--codec must be one of: qtrle, prores4444, webm"),
        })
    }

    /// Output file extension for this codec.
    pub fn ext(&self) -> &'static str {
        match self {
            Codec::Qtrle | Codec::Prores4444 => "mov",
            Codec::Webm => "webm",
        }
    }

    fn output_args(&self) -> Vec<&'static str> {
        match self {
            Codec::Qtrle => vec!["-c:v", "qtrle"],
            Codec::Prores4444 => {
                vec!["-c:v", "prores_ks", "-profile:v", "4444", "-pix_fmt", "yuva444p10le"]
            }
            Codec::Webm => vec!["-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p"],
        }
    }
}

pub struct Encoder {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Encoder {
    pub fn new(out: &Path, w: u32, h: u32, fps: f64, codec: Codec) -> Result<Encoder> {
        let size = format!("{w}x{h}");
        let fps_s = format!("{fps}");
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(["-f", "rawvideo", "-pixel_format", "rgba"])
            .args(["-video_size", &size, "-framerate", &fps_s])
            .args(["-i", "-", "-an"])
            .args(codec.output_args())
            .arg(out)
            .stdin(Stdio::piped())
            .stdout(Stdio::null());

        let mut child = cmd
            .spawn()
            .context("failed to spawn ffmpeg — is it installed and on PATH?")?;
        let stdin = child.stdin.take().context("failed to open ffmpeg stdin")?;
        Ok(Encoder { child, stdin: Some(stdin) })
    }

    pub fn write_frame(&mut self, rgba: &[u8]) -> Result<()> {
        self.stdin
            .as_mut()
            .expect("stdin taken")
            .write_all(rgba)
            .context("failed writing frame to ffmpeg (it may have exited early)")?;
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        // Close stdin so ffmpeg flushes and exits.
        self.stdin.take();
        let status = self.child.wait().context("waiting for ffmpeg")?;
        if !status.success() {
            bail!("ffmpeg exited with {status}");
        }
        Ok(())
    }
}
