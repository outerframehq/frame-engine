// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! The engine's intro, shown by a game build before its first scene: the
//! Frame Engine mark comes into frame out of a few large blocks, sharpening
//! step by step until the real image stands there, the name fades in under
//! it, then the whole thing clears to reveal the game. About four seconds;
//! any key or click skips to the end.
//!
//! It glitches on the way: each sharpening step lands with a burst (bands
//! of the mark torn sideways, red and cyan fringes, a flicker, stray
//! specks), settling as the image comes clean and flaring once more as it
//! clears. The glitches come from `glitch_at`, which hashes a tick number,
//! so the same moment always glitches the same way. A sound
//! (`SOUND_WAV`, made for it and timed to the same steps) plays over it.
//!
//! The world loads and runs behind it, so it costs no start-up time. Every
//! game build shows it; the editor's own Play never does.
//!
//! To change the picture, change `MARK_PNG`: anything dark on a light or
//! clear background works, since the mark is read as a mask (`Mask`) and
//! drawn in `INK` over `BACKDROP`. The timing lives in the constants below
//! and `Intro::frame`, which is pure, so the whole sequence is testable
//! without a window.

use egui::{Color32, Pos2, Rect, Vec2};

/// The picture that comes into frame.
const MARK_PNG: &[u8] = include_bytes!("../assets/frame-editor.png");
/// The sound played over the intro: timed to `LEVELS` stepping every
/// `LEVEL_SECONDS` from `FIRST_LEVEL`, the image landing at `sharp_at` and
/// the clear at `clear_at`, so change it with them.
pub(crate) const SOUND_WAV: &[u8] = include_bytes!("../assets/intro.wav");

/// How many times a second the glitches change.
const GLITCH_RATE: f64 = 24.0;
/// How many horizontal bands the mark can tear into.
const BANDS: usize = 16;
/// How far a torn band moves at most, as a share of the mark's width.
const TEAR: f32 = 0.12;
/// How far the colour fringes sit from the mark at most, likewise.
const SPLIT: f32 = 0.03;
/// How quickly a step's burst dies away (s).
const BURST: f64 = 0.09;
const RED: Color32 = Color32::from_rgb(255, 40, 70);
const CYAN: Color32 = Color32::from_rgb(40, 230, 255);
/// Cells across the mask: how fine the finest blocks are.
const MASK_CELLS: usize = 64;
/// Block sizes in cells, coarsest first; the last is one cell.
const LEVELS: [usize; 5] = [16, 8, 4, 2, 1];

/// When the first level appears (s), and how long each level lasts.
const FIRST_LEVEL: f64 = 0.3;
const LEVEL_SECONDS: f64 = 0.42;
/// How long a level takes to cross-fade in.
const BLEND: f64 = 0.22;
/// When the name starts fading in, and over how long.
const NAME_AT: f64 = 1.7;
const NAME_FADE: f64 = 0.6;
/// How long the finished mark holds before clearing.
const HOLD: f64 = 1.0;
/// How long the clear to the game takes.
const CLEAR: f64 = 0.55;
/// How much bigger the mark starts than it ends (it comes in from close).
const START_SCALE: f32 = 1.45;
/// The mark's width as a share of the shorter screen side.
const MARK_SHARE: f32 = 0.3;

const BACKDROP: Color32 = Color32::from_rgb(10, 10, 10);
const INK: Color32 = Color32::from_rgb(236, 232, 222);
const NAME: &str = "FRAME ENGINE";

/// When the real image has fully replaced the blocks.
fn sharp_at() -> f64 {
    FIRST_LEVEL + LEVELS.len() as f64 * LEVEL_SECONDS
}

/// When the clear begins.
fn clear_at() -> f64 {
    sharp_at().max(NAME_AT + NAME_FADE) + HOLD
}

/// How long the whole intro runs.
pub(crate) fn total_seconds() -> f64 {
    clear_at() + CLEAR
}

/// How dark each cell of the mark is, 0 (clear) to 1 (solid), on a square
/// grid of `MASK_CELLS`.
struct Mask {
    cells: Vec<f32>,
}

impl Mask {
    /// Read `png` as a mask: dark pixels count, light or clear ones don't.
    fn from_png(png: &[u8]) -> Option<Mask> {
        let image = image::load_from_memory(png).ok()?.to_rgba8();
        let (w, h) = image.dimensions();
        let (w, h) = (w as usize, h as usize);
        let pixels = image.as_raw();
        let mut cells = vec![0.0f32; MASK_CELLS * MASK_CELLS];
        for (cy, row) in cells.chunks_mut(MASK_CELLS).enumerate() {
            let y0 = cy * h / MASK_CELLS;
            let y1 = ((cy + 1) * h / MASK_CELLS).max(y0 + 1).min(h);
            for (cx, cell) in row.iter_mut().enumerate() {
                let x0 = cx * w / MASK_CELLS;
                let x1 = ((cx + 1) * w / MASK_CELLS).max(x0 + 1).min(w);
                let mut sum = 0.0;
                let mut n = 0.0;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let p = &pixels[(y * w + x) * 4..(y * w + x) * 4 + 4];
                        let light = (f32::from(p[0]) + f32::from(p[1]) + f32::from(p[2])) / 765.0;
                        let alpha = f32::from(p[3]) / 255.0;
                        sum += (1.0 - light) * alpha;
                        n += 1.0;
                    }
                }
                *cell = if n > 0.0 { sum / n } else { 0.0 };
            }
        }
        Some(Mask { cells })
    }

    /// How dark the block of `size` cells at block `(bx, by)` is: the mean
    /// of its cells.
    fn block(&self, size: usize, bx: usize, by: usize) -> f32 {
        let mut sum = 0.0;
        let mut n = 0.0;
        for y in by * size..((by + 1) * size).min(MASK_CELLS) {
            for x in bx * size..((bx + 1) * size).min(MASK_CELLS) {
                sum += self.cells[y * MASK_CELLS + x];
                n += 1.0;
            }
        }
        if n > 0.0 { sum / n } else { 0.0 }
    }
}

/// What one moment of the intro looks like.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Frame {
    /// The level of blocks on show (index into `LEVELS`), and how far the
    /// next finer one has faded in over it (0 to 1).
    pub level: usize,
    pub next_in: f32,
    /// How far the real image has replaced the blocks.
    pub sharp: f32,
    /// How much bigger than its final size the mark is.
    pub scale: f32,
    /// How visible the mark is, the name, and the whole overlay.
    pub mark: f32,
    pub name: f32,
    pub overlay: f32,
    /// Whether the intro is over.
    pub done: bool,
}

/// How the mark is glitching at one moment.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Glitch {
    /// How strong it is, 0 (clean) to about 1.
    pub amount: f32,
    /// How far each band is torn sideways, as a share of `TEAR`.
    pub shifts: [f32; BANDS],
    /// How far the colour fringes sit out, as a share of `SPLIT`.
    pub split: f32,
    /// How visible the mark is this tick (a flicker dips it).
    pub flicker: f32,
    /// Seeds the stray specks this tick; how many depends on `amount`.
    pub seed: u64,
}

/// A number from 0 to 1 for `(tick, salt)`, the same every time.
fn hash(tick: u64, salt: u64) -> f32 {
    let mut x = tick
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(salt.wrapping_mul(0xBF58_476D_1CE4_E5B9));
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 40) as f32 / (1u64 << 24) as f32
}

/// The burst after a moment `at`: full on it, dying away over `BURST`.
fn burst(t: f64, at: f64) -> f64 {
    if t < at {
        0.0
    } else {
        (-(t - at) / BURST).exp()
    }
}

/// How the mark glitches `t` seconds in: a low hum while it is blocks, a
/// burst on each step, one as the image lands, then clean while it holds
/// and a last flare as it clears.
pub(crate) fn glitch_at(t: f64) -> Glitch {
    let tick = (t.max(0.0) * GLITCH_RATE).floor() as u64;
    let mut amount = 0.0;
    if t >= FIRST_LEVEL * 0.5 && t < sharp_at() {
        amount += 0.3;
    }
    for k in 0..=LEVELS.len() {
        amount += burst(t, FIRST_LEVEL + k as f64 * LEVEL_SECONDS);
    }
    amount += 0.8 * burst(t, clear_at());
    if t >= clear_at() {
        amount += 0.5 * f64::from(ramp(clear_at(), CLEAR * 0.5, t));
    }
    // Some ticks quieter than others, so it stutters rather than buzzes.
    let amount = (amount * (0.55 + 0.45 * f64::from(hash(tick, 1)))).min(1.0) as f32;
    let mut shifts = [0.0; BANDS];
    if amount > 0.02 {
        for (i, shift) in shifts.iter_mut().enumerate() {
            if hash(tick, 10 + i as u64) < amount * 0.45 {
                *shift = (hash(tick, 40 + i as u64) * 2.0 - 1.0) * amount;
            }
        }
    }
    Glitch {
        amount,
        shifts,
        split: amount * (0.4 + 0.6 * hash(tick, 2)),
        flicker: if hash(tick, 3) < amount * 0.12 {
            0.35
        } else {
            1.0
        },
        seed: tick,
    }
}

fn ease_out(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    1.0 - (1.0 - x) * (1.0 - x) * (1.0 - x)
}

fn smooth(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn ramp(from: f64, over: f64, t: f64) -> f32 {
    smooth((t - from) / over) as f32
}

/// The intro's state: when it started, and whether it was skipped.
pub(crate) struct Intro {
    started: std::time::Instant,
    /// How far in the clock was moved when the player skipped, so the clear
    /// still plays from where it was jumped to.
    skipped_at: Option<f64>,
    mask: Option<Mask>,
    texture: Option<egui::TextureHandle>,
}

impl Intro {
    pub(crate) fn new() -> Intro {
        Intro {
            started: std::time::Instant::now(),
            skipped_at: None,
            mask: Mask::from_png(MARK_PNG),
            texture: None,
        }
    }

    /// Seconds into the sequence, with a skip jumping to the clear.
    fn elapsed(&self) -> f64 {
        let real = self.started.elapsed().as_secs_f64();
        match self.skipped_at {
            Some(at) if real < clear_at() => clear_at() + (real - at),
            _ => real,
        }
    }

    /// Jump to the clear: what any key or click does.
    pub(crate) fn skip(&mut self) {
        let real = self.started.elapsed().as_secs_f64();
        if self.skipped_at.is_none() && real < clear_at() {
            self.skipped_at = Some(real);
        }
    }

    /// Whether the intro has finished.
    #[cfg(test)]
    fn done(&self) -> bool {
        self.frame_at(self.elapsed()).done
    }

    /// The look at `t` seconds in.
    pub(crate) fn frame_at(&self, t: f64) -> Frame {
        let steps = ((t - FIRST_LEVEL) / LEVEL_SECONDS).floor();
        let level = steps.clamp(0.0, (LEVELS.len() - 1) as f64) as usize;
        let into = t - FIRST_LEVEL - level as f64 * LEVEL_SECONDS;
        let next_in = if level + 1 < LEVELS.len() && steps >= 0.0 {
            // Where the next level is already fading in over this one.
            smooth((into - (LEVEL_SECONDS - BLEND)) / BLEND) as f32
        } else {
            0.0
        };
        let sharp = ramp(sharp_at() - BLEND, BLEND, t);
        let scale = 1.0 + (START_SCALE - 1.0) * (1.0 - ease_out(t / sharp_at()) as f32);
        let clear = ramp(clear_at(), CLEAR, t);
        Frame {
            level,
            next_in,
            sharp,
            scale,
            mark: ramp(0.0, FIRST_LEVEL, t) * (1.0 - clear),
            name: ramp(NAME_AT, NAME_FADE, t) * (1.0 - clear),
            overlay: 1.0 - clear,
            done: t >= total_seconds(),
        }
    }

    /// Draw this frame over the whole screen. Returns true once the intro
    /// is over (the caller then drops it).
    pub(crate) fn show(&mut self, ctx: &egui::Context) -> bool {
        let frame = self.frame_at(self.elapsed());
        if frame.done {
            return true;
        }
        ctx.request_repaint();
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("frame-engine-intro"),
        ));
        painter.rect_filled(screen, 0.0, fade(BACKDROP, frame.overlay));

        // The mark: a square in the middle, a little above centre to leave
        // room for the name.
        let side = screen.width().min(screen.height()) * MARK_SHARE * frame.scale;
        let centre = screen.center() - Vec2::new(0.0, screen.height() * 0.05);
        let top_left = centre - Vec2::splat(side * 0.5);
        let cell = side / MASK_CELLS as f32;

        let glitch = glitch_at(self.elapsed());
        let mark = frame.mark * glitch.flicker;
        if frame.sharp > 0.0 && self.texture.is_none() {
            self.texture = Some(ctx.load_texture(
                "frame-engine-intro",
                ink_image(),
                egui::TextureOptions::LINEAR,
            ));
        }
        let place = Place {
            top_left,
            side,
            cell,
        };
        // The fringes first, the mark over them: a red copy one way and a
        // cyan one the other, only while it glitches.
        if glitch.split > 0.02 {
            let dx = glitch.split * SPLIT * side;
            let fringe = mark * glitch.amount.min(1.0) * 0.7;
            self.draw_mark(&painter, &frame, &glitch, place, -dx, RED, fringe);
            self.draw_mark(&painter, &frame, &glitch, place, dx, CYAN, fringe);
        }
        self.draw_mark(&painter, &frame, &glitch, place, 0.0, INK, mark);
        // Stray specks round the mark while it glitches hard.
        let specks = (glitch.amount * 7.0) as u64;
        for k in 0..specks {
            let x = hash(glitch.seed, 100 + k) * 1.6 - 0.3;
            let y = hash(glitch.seed, 200 + k) * 1.6 - 0.3;
            let size = cell * (1.0 + 3.0 * hash(glitch.seed, 300 + k)).round();
            let min = top_left + Vec2::new(x * side, y * side);
            let colour = match k % 3 {
                0 => RED,
                1 => CYAN,
                _ => INK,
            };
            painter.rect_filled(
                Rect::from_min_size(min, Vec2::new(size * 2.0, size)),
                0.0,
                fade(colour, frame.mark * glitch.amount * 0.8),
            );
        }
        if frame.name > 0.0 {
            let size = (screen.height() * 0.034).max(14.0);
            let font = egui::FontId::proportional(size);
            let spaced: String = NAME.chars().flat_map(|c| [c, '\u{2009}']).collect();
            let at = Pos2::new(centre.x, top_left.y + side + size * 1.6);
            if glitch.split > 0.02 {
                let dx = glitch.split * SPLIT * side * 0.6;
                for (colour, dx) in [(RED, -dx), (CYAN, dx)] {
                    painter.text(
                        at + Vec2::new(dx, 0.0),
                        egui::Align2::CENTER_CENTER,
                        spaced.trim_end(),
                        font.clone(),
                        fade(colour, frame.name * glitch.amount * 0.7),
                    );
                }
            }
            painter.text(
                at,
                egui::Align2::CENTER_CENTER,
                spaced.trim_end(),
                font,
                fade(INK, frame.name * glitch.flicker),
            );
        }
        false
    }

    /// Draw the mark as it is in `frame` (the blocks, the image over them,
    /// or both while one fades into the other) in `colour`, `alpha`
    /// strong, moved `dx` sideways and torn as `glitch` says.
    #[allow(clippy::too_many_arguments)]
    fn draw_mark(
        &self,
        painter: &egui::Painter,
        frame: &Frame,
        glitch: &Glitch,
        place: Place,
        dx: f32,
        colour: Color32,
        alpha: f32,
    ) {
        if alpha <= 0.0 {
            return;
        }
        let Place {
            top_left,
            side,
            cell,
        } = place;
        let top_left = top_left + Vec2::new(dx, 0.0);
        if let Some(mask) = &self.mask {
            let blocks = alpha * (1.0 - frame.sharp);
            if blocks > 0.0 {
                let level = |level: usize, a: f32| {
                    draw_level(painter, mask, level, top_left, cell, glitch, colour, a)
                };
                level(frame.level, blocks * (1.0 - frame.next_in));
                if frame.next_in > 0.0 && frame.level + 1 < LEVELS.len() {
                    level(frame.level + 1, blocks * frame.next_in);
                }
            }
        }
        if let Some(texture) = &self.texture
            && frame.sharp > 0.0
        {
            // In bands, each torn its own way.
            let band = side / BANDS as f32;
            for (i, shift) in glitch.shifts.iter().enumerate() {
                let y = i as f32 / BANDS as f32;
                let rect = Rect::from_min_size(
                    top_left + Vec2::new(shift * TEAR * side, i as f32 * band),
                    Vec2::new(side, band),
                );
                painter.image(
                    texture.id(),
                    rect,
                    Rect::from_min_max(Pos2::new(0.0, y), Pos2::new(1.0, y + 1.0 / BANDS as f32)),
                    fade(colour, alpha * frame.sharp),
                );
            }
        }
    }
}

/// Where the mark is drawn: its top left corner, its width, and the size of
/// one mask cell.
#[derive(Clone, Copy)]
struct Place {
    top_left: Pos2,
    side: f32,
    cell: f32,
}

/// Draw the blocks of one level in `colour`, `alpha` strong, each row torn
/// with its band.
#[allow(clippy::too_many_arguments)]
fn draw_level(
    painter: &egui::Painter,
    mask: &Mask,
    level: usize,
    top_left: Pos2,
    cell: f32,
    glitch: &Glitch,
    colour: Color32,
    alpha: f32,
) {
    if alpha <= 0.0 {
        return;
    }
    let size = LEVELS[level];
    let across = MASK_CELLS.div_ceil(size);
    let px = cell * size as f32;
    let side = cell * MASK_CELLS as f32;
    // A hair of a gap between blocks, so the coarse levels read as blocks
    // and not one slab; gone at the finest.
    let gap = if size > 1 { (px * 0.08).min(3.0) } else { 0.0 };
    for by in 0..across {
        let band = (by * size * BANDS / MASK_CELLS).min(BANDS - 1);
        let tear = glitch.shifts[band] * TEAR * side;
        for bx in 0..across {
            let dark = mask.block(size, bx, by);
            if dark < 0.08 {
                continue;
            }
            let min = top_left + Vec2::new(bx as f32 * px + tear, by as f32 * px);
            let rect = Rect::from_min_size(min, Vec2::splat(px - gap));
            painter.rect_filled(rect, 0.0, fade(colour, alpha * dark.min(1.0)));
        }
    }
}

/// `colour` at `alpha` (0 to 1) of its strength.
fn fade(colour: Color32, alpha: f32) -> Color32 {
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), a)
}

/// The mark as a white image: its dark pixels become solid, the rest
/// clear, so drawn with a tint (`INK`, or a fringe's colour) it sits on the
/// backdrop the way the blocks do.
fn ink_image() -> egui::ColorImage {
    let image = image::load_from_memory(MARK_PNG)
        .expect("the intro's embedded PNG decodes")
        .to_rgba8();
    let (w, h) = image.dimensions();
    let pixels = image
        .pixels()
        .map(|p| {
            let light = (f32::from(p[0]) + f32::from(p[1]) + f32::from(p[2])) / 765.0;
            let alpha = (1.0 - light) * f32::from(p[3]) / 255.0;
            fade(Color32::WHITE, alpha)
        })
        .collect();
    egui::ColorImage {
        size: [w as usize, h as usize],
        source_size: Vec2::new(w as f32, h as f32),
        pixels,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn intro() -> Intro {
        Intro::new()
    }

    #[test]
    fn the_mark_reads_as_a_mask_with_dark_where_the_picture_is() {
        let mask = Mask::from_png(MARK_PNG).expect("decodes");
        let dark = mask.cells.iter().filter(|&&c| c > 0.5).count();
        // Some of it is ink, most of it is clear.
        assert!(dark > 100 && dark < MASK_CELLS * MASK_CELLS / 2, "{dark}");
        // The corner brackets: the very first cell is ink.
        assert!(mask.cells[0] > 0.5);
        // A coarse block is the mean of its cells, so never darker than
        // the darkest cell in it.
        let coarse = mask.block(16, 0, 0);
        assert!(coarse > 0.0 && coarse < 1.0, "{coarse}");
    }

    #[test]
    fn the_blocks_sharpen_level_by_level_then_the_image_takes_over() {
        let intro = intro();
        let start = intro.frame_at(0.0);
        assert_eq!(start.level, 0);
        assert_eq!(start.mark, 0.0, "fades in from nothing");
        assert!(start.scale > 1.3, "starts close");
        let mut last = 0;
        let mut t = FIRST_LEVEL + 0.01;
        while t < sharp_at() {
            let f = intro.frame_at(t);
            assert!(f.level >= last, "levels only get finer");
            last = f.level;
            t += 0.1;
        }
        assert_eq!(last, LEVELS.len() - 1);
        let mid = intro.frame_at(FIRST_LEVEL + LEVEL_SECONDS - BLEND * 0.5);
        assert_eq!(mid.level, 0);
        assert!(mid.next_in > 0.0 && mid.next_in < 1.0, "cross-fading");
        let sharp = intro.frame_at(sharp_at() + 0.01);
        assert_eq!(sharp.sharp, 1.0);
        assert!((sharp.scale - 1.0).abs() < 1e-3, "at its final size");
        assert_eq!(sharp.mark, 1.0);
    }

    #[test]
    fn the_name_comes_in_the_whole_thing_holds_then_clears() {
        let intro = intro();
        assert_eq!(intro.frame_at(NAME_AT).name, 0.0);
        assert_eq!(intro.frame_at(NAME_AT + NAME_FADE).name, 1.0);
        let held = intro.frame_at(clear_at() - 0.01);
        assert_eq!((held.name, held.mark, held.overlay), (1.0, 1.0, 1.0));
        assert!(!held.done);
        let going = intro.frame_at(clear_at() + CLEAR * 0.5);
        assert!(going.overlay > 0.0 && going.overlay < 1.0);
        let over = intro.frame_at(total_seconds());
        assert_eq!(over.overlay, 0.0);
        assert!(over.done);
        assert!(
            total_seconds() > 3.0 && total_seconds() < 5.0,
            "{}",
            total_seconds()
        );
    }

    #[test]
    fn a_skip_jumps_to_the_clear_and_the_clear_still_plays() {
        let mut intro = intro();
        assert!(!intro.done());
        intro.skip();
        let t = intro.elapsed();
        assert!(t >= clear_at() && t < clear_at() + 0.1, "{t}");
        assert!(!intro.done(), "the clear still has to play");
        // Skipping again changes nothing.
        intro.skip();
        assert!(intro.elapsed() < clear_at() + 0.2);
    }

    #[test]
    fn steps_land_with_a_burst_and_the_held_mark_is_clean() {
        // Hard on each step, the same every time.
        let step = FIRST_LEVEL + 2.0 * LEVEL_SECONDS + 0.01;
        let a = glitch_at(step);
        assert_eq!(a, glitch_at(step));
        assert!(a.amount > 0.5, "{}", a.amount);
        // Quieter between steps than on them.
        let between = glitch_at(step + LEVEL_SECONDS * 0.6);
        assert!(between.amount < a.amount);
        // Clean while it holds.
        let held = glitch_at(clear_at() - 0.05);
        assert!(held.amount < 0.02, "{}", held.amount);
        assert!(held.shifts.iter().all(|&s| s == 0.0));
        assert_eq!(held.flicker, 1.0);
        // A flare as it clears.
        assert!(glitch_at(clear_at() + 0.01).amount > 0.3);
        // Some bands tear during a burst, never further than the burst.
        let torn = (0..20)
            .map(|k| glitch_at(FIRST_LEVEL + k as f64 * 0.005))
            .flat_map(|g| g.shifts)
            .filter(|&s| s != 0.0)
            .count();
        assert!(torn > 0);
        for k in 0..400 {
            let g = glitch_at(k as f64 * 0.01);
            assert!(g.shifts.iter().all(|s| s.abs() <= g.amount + 1e-6));
            assert!((0.0..=1.0).contains(&g.amount));
        }
    }

    #[test]
    fn the_sound_is_a_wav_about_as_long_as_the_intro() {
        assert_eq!(&SOUND_WAV[..4], b"RIFF");
        assert_eq!(&SOUND_WAV[8..12], b"WAVE");
        let rate = u32::from_le_bytes(SOUND_WAV[24..28].try_into().unwrap());
        let bytes_per_second = u32::from_le_bytes(SOUND_WAV[28..32].try_into().unwrap());
        let seconds = (SOUND_WAV.len() - 44) as f64 / f64::from(bytes_per_second);
        assert!(rate >= 22_050, "{rate}");
        assert!(
            seconds > total_seconds() - 0.2 && seconds < total_seconds() + 1.0,
            "{seconds}"
        );
    }

    #[test]
    fn the_ink_image_is_clear_where_the_picture_is_light() {
        let image = ink_image();
        assert_eq!(image.size, [256, 256]);
        let corner = image.pixels[0];
        let middle = image.pixels[128 * 256 + 128];
        assert!(corner.a() > 220, "the bracket: {}", corner.a());
        let clear = image.pixels[128 * 256 + 20];
        assert_eq!(clear.a(), 0, "white background");
        assert!(middle.a() > 0);
    }
}
