//! RGBA stick-overlay rendering with tiny-skia (anti-aliased, transparent bg).

use crate::log::Sample;
use anyhow::{Result, bail};
use std::collections::VecDeque;
use tiny_skia::{
    Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel {
    Roll,
    Pitch,
    Yaw,
    Throttle,
}

/// Which channel drives the X and Y axis of each stick box, per radio mode.
#[derive(Clone, Copy)]
pub struct Mapping {
    pub left: (Channel, Channel),  // (x, y)
    pub right: (Channel, Channel), // (x, y)
}

/// Standard Mode 1-4 stick assignments.
pub fn mapping(mode: u8) -> Result<Mapping> {
    use Channel::*;
    Ok(match mode {
        1 => Mapping { left: (Yaw, Pitch), right: (Roll, Throttle) },
        2 => Mapping { left: (Yaw, Throttle), right: (Roll, Pitch) },
        3 => Mapping { left: (Roll, Pitch), right: (Yaw, Throttle) },
        4 => Mapping { left: (Roll, Throttle), right: (Yaw, Pitch) },
        _ => bail!("--mode must be 1, 2, 3, or 4"),
    })
}

/// Normalization parameters (verify ranges against a real log with --info).
#[derive(Clone, Copy)]
pub struct Norm {
    pub rp_half: f32, // half-range for roll/pitch/yaw (centered on 0), default 500
    pub thr_min: f32,
    pub thr_max: f32,
    pub inv_roll: bool,
    pub inv_pitch: bool,
    pub inv_yaw: bool,
    pub inv_throttle: bool,
}

impl Default for Norm {
    fn default() -> Self {
        Norm {
            rp_half: 500.0,
            thr_min: 1000.0,
            thr_max: 2000.0,
            inv_roll: false,
            inv_pitch: false,
            inv_yaw: false,
            inv_throttle: false,
        }
    }
}

impl Norm {
    /// Map a channel to a bipolar -1..1 value where +1 means "up/right".
    fn bipolar(&self, s: &Sample, c: Channel) -> f32 {
        let (raw, inv) = match c {
            Channel::Roll => ((s.roll / self.rp_half).clamp(-1.0, 1.0), self.inv_roll),
            Channel::Pitch => ((s.pitch / self.rp_half).clamp(-1.0, 1.0), self.inv_pitch),
            Channel::Yaw => ((s.yaw / self.rp_half).clamp(-1.0, 1.0), self.inv_yaw),
            Channel::Throttle => {
                let t = ((s.throttle - self.thr_min) / (self.thr_max - self.thr_min))
                    .clamp(0.0, 1.0);
                (t * 2.0 - 1.0, self.inv_throttle)
            }
        };
        if inv { -raw } else { raw }
    }
}

/// Visual style + canvas geometry.
#[derive(Clone)]
pub struct Style {
    pub canvas_w: u32,
    pub canvas_h: u32,
    pub box_size: f32,
    pub gap: f32,
    pub pad: f32,
    pub line_width: f32,
    pub dot_radius: f32,
    pub col_border: Color,
    pub col_cross: Color,
    pub col_dot: Color,
    pub col_trail: Color,
    pub trail: usize,
}

impl Default for Style {
    fn default() -> Self {
        let box_size: f32 = 200.0;
        let gap: f32 = 40.0;
        let pad: f32 = 20.0;
        let w = (pad * 2.0 + box_size * 2.0 + gap).round() as u32;
        let h = (pad * 2.0 + box_size).round() as u32;
        Style {
            canvas_w: w,
            canvas_h: h,
            box_size,
            gap,
            pad,
            line_width: 3.0,
            dot_radius: 9.0,
            col_border: Color::from_rgba8(255, 255, 255, 200),
            col_cross: Color::from_rgba8(255, 255, 255, 90),
            col_dot: Color::from_rgba8(255, 60, 60, 255),
            col_trail: Color::from_rgba8(255, 60, 60, 255),
            trail: 0,
        }
    }
}

/// Parse "#RRGGBB", "#RRGGBBAA", or the same without the leading '#'.
pub fn parse_color(s: &str) -> Result<Color> {
    let h = s.strip_prefix('#').unwrap_or(s);
    let bytes = match h.len() {
        6 | 8 => h,
        _ => bail!("color '{s}' must be RRGGBB or RRGGBBAA hex"),
    };
    let v = u32::from_str_radix(bytes, 16).map_err(|_| anyhow::anyhow!("bad hex color '{s}'"))?;
    let (r, g, b, a) = if h.len() == 8 {
        ((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8)
    } else {
        ((v >> 16) as u8, (v >> 8) as u8, v as u8, 255)
    };
    Ok(Color::from_rgba8(r, g, b, a))
}

/// Top-left corner of a box given its index (0=left, 1=right).
struct Layout {
    left_x: f32,
    right_x: f32,
    top_y: f32,
}

pub struct Renderer {
    pixmap: Pixmap,
    style: Style,
    norm: Norm,
    map: Mapping,
    layout: Layout,
    trail: VecDeque<((f32, f32), (f32, f32))>,
    out: Vec<u8>,
}

impl Renderer {
    pub fn new(style: Style, norm: Norm, map: Mapping) -> Result<Self> {
        let pixmap = Pixmap::new(style.canvas_w, style.canvas_h)
            .ok_or_else(|| anyhow::anyhow!("invalid canvas size"))?;
        let content_w = style.box_size * 2.0 + style.gap;
        let content_h = style.box_size;
        let group_x = ((style.canvas_w as f32) - content_w) / 2.0;
        let group_y = ((style.canvas_h as f32) - content_h) / 2.0;
        let layout = Layout {
            left_x: group_x,
            right_x: group_x + style.box_size + style.gap,
            top_y: group_y,
        };
        let cap = style.canvas_w as usize * style.canvas_h as usize * 4;
        Ok(Renderer {
            pixmap,
            style,
            norm,
            map,
            layout,
            trail: VecDeque::new(),
            out: vec![0u8; cap],
        })
    }

    /// Pixel position of the dot inside a box, given its (x,y) channels.
    fn dot_pos(&self, s: &Sample, box_left: f32, axes: (Channel, Channel)) -> (f32, f32) {
        let bx = self.norm.bipolar(s, axes.0);
        let by = self.norm.bipolar(s, axes.1);
        let x_frac = (bx + 1.0) / 2.0; // 0=left .. 1=right
        let y_frac = (1.0 - by) / 2.0; // 0=top .. 1=bottom (+1 -> top)
        (
            box_left + x_frac * self.style.box_size,
            self.layout.top_y + y_frac * self.style.box_size,
        )
    }

    fn draw_box(&mut self, box_left: f32) {
        let st = &self.style;
        let stroke = Stroke { width: st.line_width, ..Default::default() };

        // Border.
        let mut paint = Paint::default();
        paint.anti_alias = true;
        paint.set_color(st.col_border);
        if let Some(rect) = Rect::from_xywh(box_left, self.layout.top_y, st.box_size, st.box_size) {
            let mut pb = PathBuilder::new();
            pb.push_rect(rect);
            if let Some(path) = pb.finish() {
                self.pixmap
                    .stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }

        // Center crosshair.
        let mut cpaint = Paint::default();
        cpaint.anti_alias = true;
        cpaint.set_color(st.col_cross);
        let cx = box_left + st.box_size / 2.0;
        let cy = self.layout.top_y + st.box_size / 2.0;
        let mut pb = PathBuilder::new();
        pb.move_to(box_left, cy);
        pb.line_to(box_left + st.box_size, cy);
        pb.move_to(cx, self.layout.top_y);
        pb.line_to(cx, self.layout.top_y + st.box_size);
        if let Some(path) = pb.finish() {
            self.pixmap
                .stroke_path(&path, &cpaint, &stroke, Transform::identity(), None);
        }
    }

    fn draw_dot(&mut self, x: f32, y: f32, color: Color, radius: f32) {
        let mut paint = Paint::default();
        paint.anti_alias = true;
        paint.set_color(color);
        if let Some(path) = PathBuilder::from_circle(x, y, radius) {
            self.pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            );
        }
    }

    /// Render one sample and return straight-alpha RGBA bytes (row-major).
    pub fn render(&mut self, s: &Sample) -> &[u8] {
        self.pixmap.fill(Color::TRANSPARENT);

        self.draw_box(self.layout.left_x);
        self.draw_box(self.layout.right_x);

        let lpos = self.dot_pos(s, self.layout.left_x, self.map.left);
        let rpos = self.dot_pos(s, self.layout.right_x, self.map.right);

        // Fading trail (oldest first, dimmer).
        if self.style.trail > 0 {
            let n = self.trail.len();
            let snapshot: Vec<_> = self.trail.iter().copied().collect();
            for (i, (lp, rp)) in snapshot.into_iter().enumerate() {
                let f = (i + 1) as f32 / (n + 1) as f32;
                let mut c = self.style.col_trail;
                c.set_alpha(c.alpha() * f * 0.6);
                let r = self.style.dot_radius * (0.4 + 0.6 * f);
                self.draw_dot(lp.0, lp.1, c, r);
                self.draw_dot(rp.0, rp.1, c, r);
            }
        }

        self.draw_dot(lpos.0, lpos.1, self.style.col_dot, self.style.dot_radius);
        self.draw_dot(rpos.0, rpos.1, self.style.col_dot, self.style.dot_radius);

        if self.style.trail > 0 {
            self.trail.push_back((lpos, rpos));
            while self.trail.len() > self.style.trail {
                self.trail.pop_front();
            }
        }

        // tiny-skia stores premultiplied alpha; ffmpeg rawvideo rgba expects
        // straight alpha, so demultiply each pixel.
        for (dst, px) in self.out.chunks_exact_mut(4).zip(self.pixmap.pixels()) {
            let c = px.demultiply();
            dst[0] = c.red();
            dst[1] = c.green();
            dst[2] = c.blue();
            dst[3] = c.alpha();
        }
        &self.out
    }

    pub fn save_png(&self, path: &std::path::Path) -> Result<()> {
        self.pixmap.save_png(path)?;
        Ok(())
    }

    pub fn width(&self) -> u32 {
        self.style.canvas_w
    }
    pub fn height(&self) -> u32 {
        self.style.canvas_h
    }
}
