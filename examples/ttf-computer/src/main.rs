// SPDX-License-Identifier: MIT OR Apache-2.0

//! Runs fonts whose hinting programs were compiled from C by the LLVM
//! TrueType backend (see llvm/utils/TrueType in llvm-project).
//!
//! ```text
//! cargo run -p ttf-computer -- run    FONT TEXT [--ppem N] [--outputs N]
//! cargo run -p ttf-computer -- render FONT TEXT [--size PX] [--png FILE]
//! ```
//!
//! `run` executes each glyph's program with skrifa, using the same hinting
//! mode as swash, and prints the values the program wrote with `ttf_out()`
//! as well as its pixel grid. `render` lays out and rasterizes TEXT with
//! cosmic-text, which runs the very same programs while hinting.

use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};
use skrifa::{
    instance::{LocationRef, Size},
    outline::{DrawSettings, HintingInstance, HintingMode, LcdLayout, OutlinePen},
    FontRef, MetadataProvider,
};
use std::process::exit;

/// Collects the points of every contour.
#[derive(Default)]
struct Points(Vec<Vec<(f32, f32)>>);

impl OutlinePen for Points {
    fn move_to(&mut self, x: f32, y: f32) {
        self.0.push(vec![(x, y)]);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.0.last_mut().unwrap().push((x, y));
    }
    fn quad_to(&mut self, _: f32, _: f32, x: f32, y: f32) {
        self.line_to(x, y);
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, x: f32, y: f32) {
        self.line_to(x, y);
    }
    fn close(&mut self) {}
}

/// Written by ttf-ld into the last outline point after the program returns.
/// Hinting failures (e.g. running out of instructions) make renderers fall
/// back to the unhinted outline silently, so this is how we detect them.
const COMPLETION_MARKER: i32 = 1000;

fn fixed(v: f32) -> i32 {
    (v * 64.0).round() as i32
}

fn run(data: &[u8], text: &str, ppem: f32, outputs: usize) -> Result<(), String> {
    let font = FontRef::new(data).map_err(|e| e.to_string())?;
    let outlines = font.outline_glyphs();
    // swash (and therefore cosmic-text) hints with this mode.
    let mode = HintingMode::Smooth {
        lcd_subpixel: Some(LcdLayout::Horizontal),
        preserve_linear_metrics: true,
    };
    let instance = HintingInstance::new(&outlines, Size::new(ppem), LocationRef::default(), mode)
        .map_err(|e| format!("prep failed: {e}"))?;
    let charmap = font.charmap();
    for ch in text.chars() {
        let gid = charmap
            .map(ch)
            .ok_or_else(|| format!("{ch:?} is not mapped"))?;
        let glyph = outlines.get(gid).ok_or("missing glyph")?;
        let mut pen = Points::default();
        glyph
            .draw(DrawSettings::hinted(&instance, false), &mut pen)
            .map_err(|e| format!("glyph {ch:?}: {e}"))?;
        let contours = pen.0;
        let points: Vec<(f32, f32)> = contours.iter().flatten().copied().collect();
        if points.last().map(|p| fixed(p.1)) != Some(COMPLETION_MARKER) {
            return Err(format!(
                "glyph {ch:?}: the hinting program did not complete \
                 (instruction or loop budget exceeded, or a runtime error)"
            ));
        }

        println!("{ch:?}:");
        for i in 0..outputs {
            let (lo, hi) = (points[2 + 2 * i].1, points[3 + 2 * i].1);
            let v = ((fixed(lo) as u32 & 0xffff) | ((fixed(hi) as u32 & 0xffff) << 16)) as i32;
            println!("  out[{i}] = {v}");
        }

        // Pixels are the 4-point contours after the reference contour.
        let pixels: Vec<&Vec<(f32, f32)>> = contours[1..]
            .iter()
            .take_while(|c| c.len() == 4)
            .collect();
        let mut xs: Vec<i32> = pixels.iter().map(|c| fixed(c[0].0)).collect();
        xs.sort();
        xs.dedup();
        let w = xs.len().max(1);
        for row in pixels.chunks(w) {
            let line: String = row
                .iter()
                .map(|c| if c[2].1 != c[0].1 { '#' } else { '.' })
                .collect();
            println!("  {line}");
        }
    }
    Ok(())
}

fn render(data: Vec<u8>, text: &str, size: f32, png: Option<&str>) {
    let mut font_system = FontSystem::new();
    font_system.db_mut().load_font_data(data);
    let mut swash_cache = SwashCache::new();
    let metrics = Metrics::new(size, size * 1.25);
    let mut buffer = Buffer::new(&mut font_system, metrics);
    let mut buffer = buffer.borrow_with(&mut font_system);
    let width = (text.chars().count() as f32 * size).max(16.0);
    buffer.set_size(Some(width), None);
    let attrs = Attrs::new().family(Family::Name("TTF Compute"));
    buffer.set_text(text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(true);

    let height = (metrics.line_height * buffer.layout_runs().count() as f32).ceil() as usize;
    let mut canvas = vec![vec![0u8; width as usize]; height];
    buffer.draw(&mut swash_cache, Color::rgb(0xff, 0xff, 0xff), |x, y, w, h, color| {
        for yy in y..y + h as i32 {
            for xx in x..x + w as i32 {
                if xx >= 0 && yy >= 0 && (yy as usize) < height && (xx as usize) < width as usize
                {
                    canvas[yy as usize][xx as usize] = color.a();
                }
            }
        }
    });
    if let Some(path) = png {
        let mut pixmap = tiny_skia::Pixmap::new(width as u32, height as u32).unwrap();
        for (y, row) in canvas.iter().enumerate() {
            for (x, &a) in row.iter().enumerate() {
                let v = 255 - a;
                pixmap.pixels_mut()[y * width as usize + x] =
                    tiny_skia::PremultipliedColorU8::from_rgba(v, v, v, 255).unwrap();
            }
        }
        pixmap.save_png(path).expect("write png");
        return;
    }
    for row in canvas {
        let line: String = row
            .iter()
            .map(|&a| match a {
                0..=31 => ' ',
                32..=127 => '.',
                128..=223 => '+',
                _ => '#',
            })
            .collect();
        println!("{}", line.trim_end());
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let usage = "usage: ttf-computer (run|render) FONT TEXT [--ppem N] [--outputs N] [--size PX] [--png FILE]";
    if args.len() < 4 {
        eprintln!("{usage}");
        exit(2);
    }
    let opt = |name: &str, default: f32| -> f32 {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .map(|v| v.parse().expect("number"))
            .unwrap_or(default)
    };
    let data = std::fs::read(&args[2]).unwrap_or_else(|e| {
        eprintln!("{}: {e}", args[2]);
        exit(1)
    });
    match args[1].as_str() {
        "run" => {
            if let Err(e) = run(&data, &args[3], opt("--ppem", 16.0), opt("--outputs", 8.0) as usize) {
                eprintln!("error: {e}");
                exit(1);
            }
        }
        "render" => {
            let png = args
                .iter()
                .position(|a| a == "--png")
                .and_then(|i| args.get(i + 1));
            render(data, &args[3], opt("--size", 32.0), png.map(|s| s.as_str()))
        }
        _ => {
            eprintln!("{usage}");
            exit(2);
        }
    }
}
