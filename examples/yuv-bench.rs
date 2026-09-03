// SPDX-License-Identifier: MIT OR GPL-2.0-only
//! How long the Android color conversion takes, per frame.
//!
//! It runs on the decode thread, so whatever it costs is subtracted from the
//! frame budget: 16.6 ms at 60 fps. A phone is slower than whatever runs this,
//! so treat the numbers as a floor and a way to compare changes.

use std::time::Instant;

use drone_app::video::yuv::{to_rgba, ColorSpace, Layout, PlaneLayout};

fn main() {
    for (w, h) in [(1280u32, 720u32), (1920, 1080)] {
        for layout in [Layout::Nv12, Layout::I420] {
            let plane = PlaneLayout {
                width: w,
                height: h,
                stride: w,
                slice_height: h,
                crop_x: 0,
                crop_y: 0,
                layout,
                color_space: ColorSpace::Bt709,
            };
            // Luma plane plus a half-size chroma plane, as the codec emits.
            let buffer = vec![0x80u8; (w * h) as usize * 3 / 2];
            let mut out = Vec::new();

            // One run to warm the allocation, then the measurement.
            to_rgba(&buffer, &plane, &mut out);

            let runs = 60;

            // As the decode loop does it today: the buffer is handed to the
            // UI and the next frame starts from nothing, so every frame
            // reallocates and zero-fills before writing every byte anyway.
            let start = Instant::now();
            for _ in 0..runs {
                let mut fresh = std::mem::take(&mut out);
                fresh.clear();
                out = fresh;
                to_rgba(&buffer, &plane, &mut out);
            }
            let fresh_each = start.elapsed() / runs;

            // With the buffer handed back and reused.
            let start = Instant::now();
            for _ in 0..runs {
                to_rgba(&buffer, &plane, &mut out);
            }
            let reused_each = start.elapsed() / runs;

            // What the UI thread then does with the result. It is a second
            // pass over the same number of pixels, so the frame budget is
            // both of these and not just the conversion.
            let start = Instant::now();
            for _ in 0..runs {
                let image =
                    egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &out);
                std::hint::black_box(&image);
            }
            let upload_each = start.elapsed() / runs;

            let total = fresh_each + upload_each;
            println!(
                "{w}x{h} {layout:?}: convert {:>5.2} ms (reused {:>5.2}) + \
                 ColorImage {:>5.2} ms = {:>5.2} ms -> {:>4.0} fps ceiling",
                fresh_each.as_secs_f64() * 1e3,
                reused_each.as_secs_f64() * 1e3,
                upload_each.as_secs_f64() * 1e3,
                total.as_secs_f64() * 1e3,
                1.0 / total.as_secs_f64(),
            );
        }
    }
}
