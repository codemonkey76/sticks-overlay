//! RGBA stick-overlay rendering with tiny-skia (anti-aliased, transparent bg).

use crate::log::Sample;
use anyhow::{Result, bail};
use tiny_skia::{
    BlendMode, Color, FillRule, Mask, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Rect, Stroke,
    Transform,
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
    pub corner_radius: f32,
    pub col_border: Color,
    pub col_cross: Color,
    pub col_dot: Color,
    pub col_fill: Color,
    pub col_trail: Color,
    /// Phosphor-decay motion trail.
    pub trail: bool,
    /// Per-frame alpha multiplier for the trail layer (0..1, higher = longer smear).
    pub trail_decay: f32,
    /// Max trail opacity when a dot is freshly stamped (0..1).
    pub trail_alpha: f32,
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
            corner_radius: 14.0,
            col_border: Color::from_rgba8(255, 255, 255, 200),
            col_cross: Color::from_rgba8(255, 255, 255, 90),
            col_dot: Color::from_rgba8(255, 60, 60, 255),
            col_fill: Color::from_rgba8(0, 0, 0, 128),
            col_trail: Color::from_rgba8(255, 255, 255, 255),
            trail: false,
            trail_decay: 0.88,
            trail_alpha: 0.5,
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

/// Build a (possibly rounded) square box path at the given top-left corner.
fn box_path(box_left: f32, top_y: f32, size: f32, radius: f32) -> Option<Path> {
    let r = radius.clamp(0.0, size / 2.0);
    let mut pb = PathBuilder::new();
    if r <= 0.0 {
        pb.push_rect(Rect::from_xywh(box_left, top_y, size, size)?);
    } else {
        let (l, t, right, b) = (box_left, top_y, box_left + size, top_y + size);
        pb.move_to(l + r, t);
        pb.line_to(right - r, t);
        pb.quad_to(right, t, right, t + r);
        pb.line_to(right, b - r);
        pb.quad_to(right, b, right - r, b);
        pb.line_to(l + r, b);
        pb.quad_to(l, b, l, b - r);
        pb.line_to(l, t + r);
        pb.quad_to(l, t, l + r, t);
        pb.close();
    }
    pb.finish()
}

/// A canvas-sized clip mask matching one box's (rounded) rectangle.
fn box_mask(style: &Style, layout: &Layout, box_left: f32) -> Result<Mask> {
    let mut mask = Mask::new(style.canvas_w, style.canvas_h)
        .ok_or_else(|| anyhow::anyhow!("invalid mask size"))?;
    if let Some(path) = box_path(box_left, layout.top_y, style.box_size, style.corner_radius) {
        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());
    }
    Ok(mask)
}

/// Stamp a filled dot into a pixmap, optionally clipped to a box mask.
fn stamp_dot(pixmap: &mut Pixmap, x: f32, y: f32, color: Color, radius: f32, mask: Option<&Mask>) {
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.set_color(color);
    if let Some(path) = PathBuilder::from_circle(x, y, radius) {
        pixmap.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), mask);
    }
}

pub struct Renderer {
    pixmap: Pixmap,
    style: Style,
    norm: Norm,
    map: Mapping,
    layout: Layout,
    /// Persistent phosphor-decay layer (canvas-sized) + per-box clip masks.
    trail_layer: Option<Pixmap>,
    box_masks: Option<[Mask; 2]>,
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
        let (trail_layer, box_masks) = if style.trail {
            let layer = Pixmap::new(style.canvas_w, style.canvas_h)
                .ok_or_else(|| anyhow::anyhow!("invalid trail layer size"))?;
            let masks = [
                box_mask(&style, &layout, layout.left_x)?,
                box_mask(&style, &layout, layout.right_x)?,
            ];
            (Some(layer), Some(masks))
        } else {
            (None, None)
        };
        let cap = style.canvas_w as usize * style.canvas_h as usize * 4;
        Ok(Renderer {
            pixmap,
            style,
            norm,
            map,
            layout,
            trail_layer,
            box_masks,
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

        // Interior fill + rounded border share one path.
        if let Some(path) = box_path(box_left, self.layout.top_y, st.box_size, st.corner_radius) {
            let mut fill = Paint::default();
            fill.anti_alias = true;
            fill.set_color(st.col_fill);
            self.pixmap
                .fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);

            let mut paint = Paint::default();
            paint.anti_alias = true;
            paint.set_color(st.col_border);
            self.pixmap
                .stroke_path(&path, &paint, &stroke, Transform::identity(), None);
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

        // 1. Static box chrome.
        self.draw_box(self.layout.left_x);
        self.draw_box(self.layout.right_x);

        let lpos = self.dot_pos(s, self.layout.left_x, self.map.left);
        let rpos = self.dot_pos(s, self.layout.right_x, self.map.right);

        // 2. Phosphor-decay trail: fade the persistent layer, stamp the current
        //    dots into it (clipped to each box), then composite it faintly.
        if let Some(layer) = self.trail_layer.as_mut() {
            let st = &self.style;

            // Fade: DestinationIn keeps dst scaled by the source alpha (= decay),
            // so the whole layer's alpha is multiplied by `trail_decay` per frame.
            let mut fade = Paint::default();
            fade.blend_mode = BlendMode::DestinationIn;
            fade.set_color(Color::from_rgba(0.0, 0.0, 0.0, st.trail_decay).unwrap_or(Color::BLACK));
            if let Some(rect) =
                Rect::from_xywh(0.0, 0.0, st.canvas_w as f32, st.canvas_h as f32)
            {
                layer.fill_rect(rect, &fade, Transform::identity(), None);
            }

            // Stamp the current head positions at max trail opacity.
            let mut c = st.col_trail;
            c.set_alpha((c.alpha() * st.trail_alpha).clamp(0.0, 1.0));
            let masks = self.box_masks.as_ref().unwrap();
            stamp_dot(layer, lpos.0, lpos.1, c, st.dot_radius, Some(&masks[0]));
            stamp_dot(layer, rpos.0, rpos.1, c, st.dot_radius, Some(&masks[1]));

            // Composite the faint trail under the crisp head dot.
            self.pixmap.draw_pixmap(
                0,
                0,
                layer.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }

        // 3. Crisp full-opacity head dot on top.
        self.draw_dot(lpos.0, lpos.1, self.style.col_dot, self.style.dot_radius);
        self.draw_dot(rpos.0, rpos.1, self.style.col_dot, self.style.dot_radius);

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

    /// Vertical extent (px) of non-transparent trail-layer pixels in a column.
    /// The trail layer holds only the stamped dots, so this isolates the smear
    /// from the static chrome and the head dot.
    #[cfg(test)]
    fn trail_span(&self, x0: u32, x1: u32) -> u32 {
        let layer = self.trail_layer.as_ref().expect("trail enabled");
        let w = layer.width();
        let (mut min_y, mut max_y) = (u32::MAX, 0u32);
        for y in 0..layer.height() {
            for x in x0..x1.min(w) {
                // ~10% of 255: ignore the sub-perceptual phosphor tail.
                if layer.pixels()[(y * w + x) as usize].alpha() > 25 {
                    min_y = min_y.min(y);
                    max_y = max_y.max(y);
                }
            }
        }
        if max_y >= min_y { max_y - min_y } else { 0 }
    }
}

#[cfg(test)]
mod tests {
    //! Phosphor-decay trail: the smear should stretch vertically on a fast
    //! throttle chop and collapse to a tight dot when the stick is held.
    //!
    //! Mode 2 puts throttle on the LEFT box's Y axis and yaw on its X axis, so
    //! we park yaw off-center (clear of the vertical crosshair) and sweep
    //! throttle, then measure the vertical extent of white trail pixels in a
    //! column through the dot.
    use super::*;

    fn sample(throttle: f32) -> Sample {
        Sample { t_us: 0, roll: 0.0, pitch: 0.0, yaw: 250.0, throttle }
    }

    // Defaults: left box center x = 120, yaw 250/500 = 0.5 -> dot x ~ 170.
    const COL: (u32, u32) = (166, 174);

    #[test]
    fn smear_stretches_on_fast_move_and_tightens_when_held() {
        let mut style = Style::default();
        style.trail = true; // default decay 0.88, alpha 0.5
        let mut r = Renderer::new(style, Norm::default(), mapping(2).unwrap()).unwrap();

        // Hold high.
        for _ in 0..20 {
            r.render(&sample(1900.0));
        }
        let span_held_high = r.trail_span(COL.0, COL.1);

        // Fast chop high -> low over two frames; capture the smear.
        r.render(&sample(1500.0));
        r.render(&sample(1100.0));
        let span_chop = r.trail_span(COL.0, COL.1);

        // Hold low; the smear should decay back to a tight dot.
        for _ in 0..20 {
            r.render(&sample(1100.0));
        }
        let span_held_low = r.trail_span(COL.0, COL.1);

        eprintln!(
            "span held-high={span_held_high}px chop={span_chop}px held-low={span_held_low}px"
        );

        assert!(span_held_high < 40, "held-high should be tight: {span_held_high}");
        assert!(span_held_low < 40, "held-low should be tight: {span_held_low}");
        assert!(span_chop > 100, "chop should smear: {span_chop}");
        assert!(
            span_chop > span_held_low * 3,
            "smear ({span_chop}) should dwarf held dot ({span_held_low})"
        );
    }
}
