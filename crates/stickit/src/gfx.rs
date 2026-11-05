//! Drawing helpers that paint straight into a window's pixel buffer.
//!
//! Buffers hold premultiplied B,G,R,A bytes. tiny-skia writes R,G,B,A, so every colour handed
//! to tiny-skia has red and blue swapped, which lets us render in place with no extra pixmap.

use crate::theme::Rgb;
use cosmic_text::{render_decoration, Buffer, Color, Cursor, FontSystem, PhysicalGlyph, Renderer, SwashCache};
use tiny_skia::{
    FillRule, GradientStop, LineCap, LinearGradient, Paint, Path, PathBuilder, PixmapMut, Point, SpreadMode, Stroke,
    Transform,
};

/// x, y, width, height.
pub type Rect = (f32, f32, f32, f32);

#[derive(Clone, Copy)]
pub struct Clip {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

fn skia(c: Rgb, alpha: u8) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.2, c.1, c.0, alpha)
}

fn paint(c: Rgb, alpha: u8) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(skia(c, alpha));
    p.anti_alias = true;
    p
}

/// Draws `src` (a same-size buffer) over `dst` at `opacity`, scaled by `scale` about `anchor`
/// and moved down by `dy`. Used only while something is springing in.
#[allow(clippy::too_many_arguments)]
pub fn composite(dst: &mut [u8], src: &[u8], w: u32, h: u32, opacity: f32, scale: f32, anchor: (f32, f32), dy: f32) {
    let (Some(mut pm), Some(layer)) = (PixmapMut::from_bytes(dst, w, h), tiny_skia::PixmapRef::from_bytes(src, w, h))
    else {
        return;
    };
    let (ax, ay) = anchor;
    let t = tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, ax - ax * scale, ay - ay * scale + dy);
    let paint = tiny_skia::PixmapPaint {
        opacity: opacity.clamp(0.0, 1.0),
        blend_mode: tiny_skia::BlendMode::SourceOver,
        quality: tiny_skia::FilterQuality::Bilinear,
    };
    pm.draw_pixmap(0, 0, layer, &paint, t, None);
}

/// Redraws the whole buffer scaled by `scale` about `anchor` (a window popping in).
pub fn rescale(data: &mut [u8], w: u32, h: u32, scale: f32, anchor: (f32, f32)) {
    if (scale - 1.0).abs() < 0.001 {
        return;
    }
    let copy = data.to_vec();
    data.fill(0);
    composite(data, &copy, w, h, 1.0, scale, anchor, 0.0);
}

pub fn fill_rect(pm: &mut PixmapMut, x: f32, y: f32, w: f32, h: f32, c: Rgb, alpha: u8) {
    if let Some(r) = tiny_skia::Rect::from_xywh(x, y, w, h) {
        pm.fill_rect(r, &paint(c, alpha), Transform::identity(), None);
    }
}

pub fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    let r = r.min(w / 2.0).min(h / 2.0);
    let k = 0.5523 * r;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Rectangle with continuous ("squircle") corners, the shape Apple uses for app icons and
/// windows: each corner is a superellipse quadrant, so curvature eases in instead of
/// starting abruptly like a circular arc.
pub fn squircle(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    const N: usize = 12;
    const EXP: f32 = 2.0 / 4.5;
    let e = (r * 1.45).min(w / 2.0).min(h / 2.0);
    let q: Vec<(f32, f32)> = (0..=N)
        .map(|i| {
            let t = i as f32 / N as f32 * std::f32::consts::FRAC_PI_2;
            // cos(π/2) is a tiny negative in f32; a fractional power of it would be NaN.
            (t.cos().max(0.0).powf(EXP), t.sin().max(0.0).powf(EXP))
        })
        .collect();
    let mut pb = PathBuilder::new();
    pb.move_to(x + e, y);
    // Top-right, bottom-right, bottom-left, top-left; (cx, cy, sx, sy, reversed).
    let corners = [
        (x + w - e, y + e, 1.0, -1.0, true),
        (x + w - e, y + h - e, 1.0, 1.0, false),
        (x + e, y + h - e, -1.0, 1.0, true),
        (x + e, y + e, -1.0, -1.0, false),
    ];
    for (cx, cy, sx, sy, rev) in corners {
        let mut pts: Vec<&(f32, f32)> = q.iter().collect();
        if rev {
            pts.reverse();
        }
        for &&(c, s) in &pts {
            pb.line_to(cx + sx * e * c, cy + sy * e * s);
        }
    }
    pb.close();
    pb.finish()
}

pub fn fill_path(pm: &mut PixmapMut, path: &Path, c: Rgb, alpha: u8) {
    pm.fill_path(path, &paint(c, alpha), FillRule::Winding, Transform::identity(), None);
}

/// Fills `path` with a vertical gradient of `c` from `a0` alpha at `y0` to `a1` at `y1`.
pub fn fill_path_gradient(pm: &mut PixmapMut, path: &Path, c: Rgb, (y0, a0): (f32, u8), (y1, a1): (f32, u8)) {
    let stops = vec![GradientStop::new(0.0, skia(c, a0)), GradientStop::new(1.0, skia(c, a1))];
    let Some(shader) = LinearGradient::new(
        Point::from_xy(0.0, y0),
        Point::from_xy(0.0, y1),
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    ) else {
        return;
    };
    let p = Paint { shader, anti_alias: true, ..Paint::default() };
    pm.fill_path(path, &p, FillRule::Winding, Transform::identity(), None);
}

pub fn stroke_path(pm: &mut PixmapMut, path: &Path, width: f32, c: Rgb, alpha: u8) {
    let stroke = Stroke { width, line_cap: LineCap::Round, ..Stroke::default() };
    pm.stroke_path(path, &paint(c, alpha), &stroke, Transform::identity(), None);
}

/// A dotted outline: `dash` long strokes with `gap` between them.
#[allow(clippy::too_many_arguments)]
pub fn stroke_dashed(pm: &mut PixmapMut, path: &Path, width: f32, c: Rgb, alpha: u8, dash: f32, gap: f32) {
    let stroke = Stroke {
        width,
        line_cap: LineCap::Round,
        dash: tiny_skia::StrokeDash::new(vec![dash, gap], 0.0),
        ..Stroke::default()
    };
    pm.stroke_path(path, &paint(c, alpha), &stroke, Transform::identity(), None);
}

pub fn circle(cx: f32, cy: f32, r: f32) -> Option<Path> {
    PathBuilder::from_circle(cx, cy, r)
}

/// Builds a polyline path from points; `close` joins the last point to the first.
pub fn polyline(points: &[(f32, f32)], close: bool) -> Option<Path> {
    let (first, rest) = points.split_first()?;
    let mut pb = PathBuilder::new();
    pb.move_to(first.0, first.1);
    for p in rest {
        pb.line_to(p.0, p.1);
    }
    if close {
        pb.close();
    }
    pb.finish()
}

/// A soft drop shadow for a rectangle: one Gaussian layer per `(sigma, dy, alpha)`.
/// Uses the separable closed form (a blurred box is a product of two erf profiles), so it
/// costs a couple of multiplies per pixel and needs no blur buffers. Writes over a cleared buffer.
pub fn shadow(
    data: &mut [u8],
    width: u32,
    height: u32,
    rect: (f32, f32, f32, f32),
    layers: &[(f32, f32, f32)],
    hole: Option<(f32, f32, f32, f32)>,
) {
    let (w, h) = (width as usize, height as usize);
    let (x, y, rw, rh) = rect;
    // Beyond ~3σ the shadow is invisible; the buffer is already clear there.
    let reach = layers.iter().map(|&(sigma, dy, _)| 3.0 * sigma + dy.abs()).fold(0.0, f32::max) + 2.0;
    let (x0, x1) = (((x - reach).max(0.0)) as usize, ((x + rw + reach).ceil() as usize).min(w));
    let (y0, y1) = (((y - reach).max(0.0)) as usize, ((y + rh + reach).ceil() as usize).min(h));
    if x0 >= x1 || y0 >= y1 {
        return;
    }
    // Erf profiles only over the affected span (index 0 = x0 / y0).
    let profile = |from: usize, to: usize, a: f32, b: f32, sigma: f32| -> Vec<f32> {
        let k = 1.0 / (sigma * std::f32::consts::SQRT_2);
        (from..to)
            .map(|i| {
                let p = i as f32 + 0.5;
                0.5 * (erf((p - a) * k) - erf((p - b) * k))
            })
            .collect()
    };
    let layers: Vec<(Vec<f32>, Vec<f32>, f32)> = layers
        .iter()
        .map(|&(sigma, dy, alpha)| {
            (profile(x0, x1, x, x + rw, sigma), profile(y0, y1, y + dy, y + dy + rh, sigma), alpha)
        })
        .collect();
    // Pixels the opaque card will cover anyway are skipped.
    let (hx0, hy0, hx1, hy1) = hole.map_or((0, 0, 0, 0), |(hx, hy, hw, hh)| {
        (
            hx.max(0.0).ceil() as usize,
            hy.max(0.0).ceil() as usize,
            (hx + hw).max(0.0) as usize,
            (hy + hh).max(0.0) as usize,
        )
    });
    for py in y0..y1 {
        let spans = if py >= hy0 && py < hy1 && hx0 < hx1 {
            [(x0, hx0.clamp(x0, x1)), (hx1.clamp(x0, x1), x1)]
        } else {
            [(x0, x1), (x1, x1)]
        };
        for (from, to) in spans {
            for px in from..to {
                let mut a = 0.0;
                for (lx, ly, alpha) in &layers {
                    a += alpha * lx[px - x0] * ly[py - y0];
                }
                // Max-combine so several shadows (one per card) can share a buffer.
                let i = (py * w + px) * 4 + 3;
                data[i] = data[i].max((a.min(1.0) * 255.0) as u8);
            }
        }
    }
}

fn erf(x: f32) -> f32 {
    // Abramowitz & Stegun 7.1.26 (max error ~1.5e-7).
    let s = x.signum();
    let x = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let y = 1.0
        - (((((1.061_405_4 * t - 1.453_152_1) * t) + 1.421_413_8) * t - 0.284_496_74) * t + 0.254_829_6)
            * t
            * (-x * x).exp();
    s * y
}

/// Paints a laid-out buffer at (ox, oy). Unlike `Buffer::draw`, this honours the alpha of
/// span colours, which we rely on for dimmed markdown markers and hidden list syntax.
#[allow(clippy::too_many_arguments)]
pub fn draw_text(
    buffer: &mut Buffer,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    base: Color,
    data: &mut [u8],
    stride: u32,
    clip: Clip,
    (ox, oy): (i32, i32),
) {
    buffer.shape_until_scroll(fonts, false);
    let mut rects = RectSink { data, stride, clip, ox, oy };
    for run in buffer.layout_runs() {
        for glyph in run.glyphs {
            let color = glyph.color_opt.unwrap_or(base);
            if color.a() == 0 {
                continue;
            }
            let pg = glyph.physical((0.0, run.line_y), 1.0);
            swash.with_pixels(fonts, pg.cache_key, color, |x, y, c| {
                let a = (c.a() as u32 * color.a() as u32 / 255) as u8;
                let c = Color::rgba(c.r(), c.g(), c.b(), a);
                blend_rect(rects.data, stride, clip, pg.x + x + ox, pg.y + y + oy, 1, 1, c);
            });
        }
        render_decoration(&mut rects, &run, base);
    }
}

struct RectSink<'a> {
    data: &'a mut [u8],
    stride: u32,
    clip: Clip,
    ox: i32,
    oy: i32,
}

impl Renderer for RectSink<'_> {
    fn rectangle(&mut self, x: i32, y: i32, w: u32, h: u32, color: Color) {
        blend_rect(self.data, self.stride, self.clip, x + self.ox, y + self.oy, w, h, color);
    }

    fn glyph(&mut self, _: PhysicalGlyph, _: Color) {}
}

/// Rectangles covering the text between two cursors, in buffer coordinates.
pub fn span_rects(buffer: &Buffer, a: Cursor, b: Cursor) -> Vec<(f32, f32, f32, f32)> {
    // `LayoutRun::highlight` treats runs outside [a.line, b.line] as fully selected.
    buffer
        .layout_runs()
        .filter(|run| run.line_i >= a.line && run.line_i <= b.line)
        .flat_map(|run| {
            let (top, h) = (run.line_top, run.line_height);
            run.highlight(a, b).map(move |(x, w)| (x, top, w, h)).collect::<Vec<_>>()
        })
        .collect()
}

/// Source-over blend of a straight-alpha colour into the premultiplied BGRA buffer.
#[allow(clippy::too_many_arguments)]
pub fn blend_rect(data: &mut [u8], stride: u32, clip: Clip, x: i32, y: i32, w: u32, h: u32, color: Color) {
    let a = color.a() as u32;
    if a == 0 {
        return;
    }
    let inv = 255 - a;
    let (b, g, r) = (color.b() as u32 * a, color.g() as u32 * a, color.r() as u32 * a);
    // Callers' clips can reach past the buffer (a popover taller than its window); never write there.
    let rows = (data.len() / 4 / stride.max(1) as usize) as i32;
    let x0 = x.max(clip.x0).max(0);
    let y0 = y.max(clip.y0).max(0);
    let x1 = (x + w as i32).min(clip.x1).min(stride as i32);
    let y1 = (y + h as i32).min(clip.y1).min(rows);
    for py in y0..y1 {
        let row = py as usize * stride as usize;
        for px in x0..x1 {
            let i = (row + px as usize) * 4;
            data[i] = ((b + data[i] as u32 * inv + 127) / 255) as u8;
            data[i + 1] = ((g + data[i + 1] as u32 * inv + 127) / 255) as u8;
            data[i + 2] = ((r + data[i + 2] as u32 * inv + 127) / 255) as u8;
            data[i + 3] = ((a * 255 + data[i + 3] as u32 * inv + 127) / 255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn blend_stays_inside_the_buffer() {
        let mut data = vec![0u8; 4 * 10 * 10];
        let clip = super::Clip { x0: -5, y0: -5, x1: 50, y1: 50 };
        super::blend_rect(&mut data, 10, clip, -3, 8, 20, 20, cosmic_text::Color::rgba(255, 0, 0, 255));
        assert_eq!(data[(9 * 10 + 9) * 4 + 3], 255);
    }

    #[test]
    fn squircle_is_a_valid_path() {
        let p = super::squircle(10.0, 10.0, 200.0, 150.0, 18.0).expect("path");
        let b = p.bounds();
        assert!((b.left() - 10.0).abs() < 0.01 && (b.right() - 210.0).abs() < 0.01);
        assert!((b.top() - 10.0).abs() < 0.01 && (b.bottom() - 160.0).abs() < 0.01);
    }

    #[test]
    fn composite_respects_opacity() {
        let (w, h) = (4u32, 4u32);
        let src = vec![255u8; (w * h * 4) as usize];
        let mut dst = vec![0u8; src.len()];
        super::composite(&mut dst, &src, w, h, 0.0, 1.0, (0.0, 0.0), 0.0);
        assert!(dst.iter().all(|&b| b == 0));
        super::composite(&mut dst, &src, w, h, 1.0, 1.0, (0.0, 0.0), 0.0);
        assert!(dst.iter().all(|&b| b == 255));
    }

    #[test]
    fn rescale_at_full_size_changes_nothing() {
        let mut data: Vec<u8> = (0..64).map(|i| (i * 3) as u8).collect();
        let before = data.clone();
        super::rescale(&mut data, 4, 4, 1.0, (2.0, 2.0));
        assert_eq!(data, before);
    }
}
