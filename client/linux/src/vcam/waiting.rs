//! The picture the virtual camera shows while the sender is away: the AV Relay
//! icon and name, and "Waiting for connection…", drawn with cairo and pango
//! and converted to the camera's YUYV. It names no device: everyone in the
//! call sees it.

use anyhow::{Context, Result, anyhow};
use cairo::{Context as Cairo, Format, ImageSurface, LinearGradient};

use super::CameraFormat;

const ICON_PNG: &[u8] = include_bytes!("../../../../assets/generated/app-icon-256.png");

/// A YUYV frame of `format` saying AV Relay is waiting for its connection.
pub fn waiting_frame(format: CameraFormat) -> Result<Vec<u8>> {
    let (w, h) = (format.width as i32, format.height as i32);
    let mut surface = ImageSurface::create(Format::Rgb24, w, h).map_err(|e| anyhow!("cairo surface: {e}"))?;
    {
        let cr = Cairo::new(&surface).map_err(|e| anyhow!("cairo context: {e}"))?;
        draw(&cr, w as f64, h as f64)?;
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().map_err(|e| anyhow!("cairo data: {e}"))?;
    Ok(rgb24_to_yuyv(&data, w as usize, h as usize, stride))
}

fn draw(cr: &Cairo, w: f64, h: f64) -> Result<()> {
    // A dark background with a hint of the logo's blue.
    let bg = LinearGradient::new(0.0, 0.0, w, h);
    bg.add_color_stop_rgb(0.0, 0.07, 0.09, 0.16);
    bg.add_color_stop_rgb(1.0, 0.04, 0.12, 0.15);
    cr.set_source(&bg).ok();
    cr.paint().ok();

    // The icon, centred above the text.
    let icon = ImageSurface::create_from_png(&mut std::io::Cursor::new(ICON_PNG)).context("decoding the icon")?;
    let size = h * 0.22;
    let scale = size / icon.height() as f64;
    let (x, y) = ((w - size) / 2.0, h * 0.24);
    cr.save().ok();
    cr.translate(x, y);
    cr.scale(scale, scale);
    cr.set_source_surface(&icon, 0.0, 0.0).ok();
    cr.source().set_filter(cairo::Filter::Best);
    cr.paint().ok();
    cr.restore().ok();

    let line = |text: &str, px: f64, weight: pango::Weight, alpha: f64, top: f64| {
        let layout = pangocairo::functions::create_layout(cr);
        let mut font = pango::FontDescription::from_string("Sans");
        font.set_absolute_size(px * pango::SCALE as f64);
        font.set_weight(weight);
        layout.set_font_description(Some(&font));
        layout.set_text(text);
        let (_, logical) = layout.pixel_extents();
        cr.move_to((w - logical.width() as f64) / 2.0, top);
        cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
        pangocairo::functions::show_layout(cr, &layout);
    };
    line("AV Relay", h * 0.06, pango::Weight::Bold, 1.0, y + size + h * 0.05);
    line("Waiting for connection…", h * 0.035, pango::Weight::Normal, 0.75, y + size + h * 0.15);
    Ok(())
}

/// Cairo's RGB24 (one u32 per pixel, 0x00RRGGBB in native order) to YUYV,
/// BT.709 limited range, chroma averaged over each pixel pair.
fn rgb24_to_yuyv(data: &[u8], w: usize, h: usize, stride: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 2);
    for row in 0..h {
        let line = &data[row * stride..row * stride + w * 4];
        for pair in line.chunks_exact(8) {
            let px = |i: usize| {
                let v = u32::from_ne_bytes([pair[i * 4], pair[i * 4 + 1], pair[i * 4 + 2], pair[i * 4 + 3]]);
                (((v >> 16) & 0xff) as f32, ((v >> 8) & 0xff) as f32, (v & 0xff) as f32)
            };
            let (r0, g0, b0) = px(0);
            let (r1, g1, b1) = px(1);
            let y = |r: f32, g: f32, b: f32| 16.0 + 0.1826 * r + 0.6142 * g + 0.0620 * b;
            let (r, g, b) = ((r0 + r1) / 2.0, (g0 + g1) / 2.0, (b0 + b1) / 2.0);
            let u = 128.0 - 0.1006 * r - 0.3386 * g + 0.4392 * b;
            let v = 128.0 + 0.4392 * r - 0.3989 * g - 0.0403 * b;
            out.extend_from_slice(&[
                y(r0, g0, b0).round() as u8,
                u.round().clamp(16.0, 240.0) as u8,
                y(r1, g1, b1).round() as u8,
                v.round().clamp(16.0, 240.0) as u8,
            ]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_the_exact_frame_size() {
        for (width, height) in [(1920, 1080), (1280, 720)] {
            let format = CameraFormat { width, height, fps: 30 };
            let frame = waiting_frame(format).unwrap();
            assert_eq!(frame.len(), format.frame_size());
            // Not just the black placeholder: the icon and text are there.
            assert!(frame.chunks_exact(2).any(|p| p[0] > 200), "no bright pixels");
            // To look at it: AV_RELAY_DUMP_WAITING=/tmp/w cargo test, then
            // ffmpeg -f rawvideo -pix_fmt yuyv422 -s WxH -i /tmp/w-WxH.yuv out.png
            if let Some(dir) = std::env::var_os("AV_RELAY_DUMP_WAITING") {
                std::fs::write(format!("{}-{width}x{height}.yuv", dir.to_string_lossy()), &frame).unwrap();
            }
        }
    }

    #[test]
    fn white_and_black_convert_to_limited_range() {
        let px = |rgb: u32| rgb.to_ne_bytes();
        let data = [px(0xffffff), px(0xffffff), px(0x000000), px(0x000000)].concat();
        let out = rgb24_to_yuyv(&data, 4, 1, 16);
        assert_eq!(out, vec![235, 128, 235, 128, 16, 128, 16, 128]);
    }
}
