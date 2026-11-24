/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Text layout and font rasterization abstraction.
//!
//! This is implemented using the [rusttype] library. All usage of that library
//! should be confined to this module.
//!
//! TODO: Less terrible text layout. RustType doesn't do text layout so this
//! code has its own, not particularly good implementation. We might want to
//! switch to something like cosmic-text in future, but that has a _lot_ more
//! dependencies.

use crate::frameworks::core_graphics::cg_affine_transform::CGAffineTransform;
use crate::frameworks::core_graphics::{CGPoint, CGRect, CGSize};
use crate::paths;
use owned_ttf_parser::AsFaceRef;
use rusttype::{vector, GlyphId, Point, Scale};
use std::io::Read;

pub struct Font {
    font: rusttype::Font<'static>,
    scale_factor: f32,
    /// n72 fork: precomputed ASCII 0..128 glyph IDs and raw horizontal metrics
    /// (advance, left side bearing) in font units, avoiding per-char cmap/hmtx
    /// table walks on every frame.
    ascii_glyph_ids: [GlyphId; 128],
    ascii_h_metrics_raw: [(u16, i16); 128],
    has_kern: bool,
    /// Lazy cache of raw horizontal kerning (in font units) for ASCII pairs,
    /// using `i32::MIN` as the unpopulated sentinel.
    ascii_kern_raw: std::cell::RefCell<Option<Box<[i32; 128 * 128]>>>,
    /// n72 fork: rasterized glyphs, so text drawn again (every frame, for a
    /// scrolling ticker) is not re-rasterized from its outline each time.
    glyph_cache: std::cell::RefCell<GlyphCache>,
}

/// Key: glyph id, scale (f32 bits), x and y sub-pixel position in quarters.
type GlyphKey = (u16, u32, u8, u8);

/// A rasterized glyph: bounds relative to the integer pen position, and
/// coverage (None for glyphs without pixels, e.g. a space).
type CachedGlyph = Option<((i32, i32), (usize, usize), Vec<f32>)>;

#[derive(Default)]
struct FxBuildHasher;

struct FxHasher(u64);

impl std::hash::BuildHasher for FxBuildHasher {
    type Hasher = FxHasher;
    fn build_hasher(&self) -> Self::Hasher {
        FxHasher(0)
    }
}

impl std::hash::Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u8(b);
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.0 = (self.0.rotate_left(5) ^ (i as u64)).wrapping_mul(0x517cc1b727220a95);
    }
    #[inline]
    fn write_u16(&mut self, i: u16) {
        self.0 = (self.0.rotate_left(5) ^ (i as u64)).wrapping_mul(0x517cc1b727220a95);
    }
    #[inline]
    fn write_u32(&mut self, i: u32) {
        self.0 = (self.0.rotate_left(5) ^ (i as u64)).wrapping_mul(0x517cc1b727220a95);
    }
    #[inline]
    fn write_u64(&mut self, i: u64) {
        self.0 = (self.0.rotate_left(5) ^ i).wrapping_mul(0x517cc1b727220a95);
    }
}

#[derive(Default)]
struct GlyphCache {
    map: std::collections::HashMap<GlyphKey, CachedGlyph, FxBuildHasher>,
}

impl GlyphCache {
    /// Above this many entries the cache starts again (a few MB at most).
    const MAX_ENTRIES: usize = 2048;
}

pub enum TextAlignment {
    Left,
    Center,
    Right,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum WrapMode {
    Word,
    Char,
}

/// Helper for [Font::draw], used for the `draw_glyph` callback.
pub struct RasterGlyph<'a> {
    origin: (f32, f32),
    dimensions: (i32, i32),
    pixels: &'a [f32],
}
impl RasterGlyph<'_> {
    /// Get the x and y co-ordinates the glyph should be drawn at.
    pub fn origin(&self) -> (f32, f32) {
        self.origin
    }
    /// Get the dimensions, in pixels, of the glyph.
    pub fn dimensions(&self) -> (i32, i32) {
        self.dimensions
    }
    /// Get the rasterized alpha coverage pixels (row-major).
    pub fn pixels(&self) -> &[f32] {
        self.pixels
    }
    /// Get the coverage at the given co-ordinates within the glyph.
    pub fn pixel_at(&self, coords: (i32, i32)) -> f32 {
        let (width, height) = self.dimensions;
        if (0..width).contains(&coords.0) && (0..height).contains(&coords.1) {
            self.pixels[coords.1 as usize * width as usize + coords.0 as usize]
        } else {
            0.0 // safety in case of rounding errors
        }
    }
}

impl Font {
    fn scale(&self, font_size: f32) -> Scale {
        Scale::uniform(font_size * self.scale_factor)
    }

    fn v_metrics_scaled(&self, font_size: f32) -> rusttype::VMetrics {
        self.font.v_metrics(self.scale(font_size))
    }

    pub fn glyph_id_for_char(&self, c: u16) -> GlyphId {
        self.font.glyph(char::from_u32(c as u32).unwrap()).id()
    }

    fn from_rusttype(font: rusttype::Font<'static>, scale_factor: f32) -> Font {
        let face = match &font {
            rusttype::Font::Owned(f) => f.as_face_ref(),
            _ => unreachable!(),
        };
        let ascii_glyph_ids: [GlyphId; 128] = std::array::from_fn(|i| {
            let ch = char::from_u32(i as u32).unwrap();
            font.glyph(ch).id()
        });
        let ascii_h_metrics_raw: [(u16, i16); 128] = std::array::from_fn(|i| {
            let tid = owned_ttf_parser::GlyphId(ascii_glyph_ids[i].0);
            (
                face.glyph_hor_advance(tid).unwrap_or(0),
                face.glyph_hor_side_bearing(tid).unwrap_or(0),
            )
        });
        let has_kern = face.tables().kern.is_some();
        Font {
            font,
            scale_factor,
            ascii_glyph_ids,
            ascii_h_metrics_raw,
            has_kern,
            ascii_kern_raw: std::cell::RefCell::new(None),
            glyph_cache: Default::default(),
        }
    }

    fn from_resource_file(filename: &str) -> Font {
        let mut bytes = Vec::new();
        let path = format!("{}/{}", paths::FONTS_DIR, filename);
        if let Err(e) = paths::ResourceFile::open(&path)
            .and_then(|mut f| f.get().read_to_end(&mut bytes).map_err(|e| e.to_string()))
        {
            panic!(
                "Couldn't read bundled font file {path:?}: {e}. Perhaps the directory is missing?"
            );
        }

        let Some(font) = rusttype::Font::try_from_vec(bytes) else {
            panic!("Couldn't parse bundled font file {path:?}. This probably means the file is corrupt. Try re-downloading it.");
        };

        // TODO: Make this a lookup based on the actual font
        // iPhone OS's interpretation of font size is slightly different,
        // when substituting Helvetica with our Liberation font.
        // This scale factor has been eyeball'd, it's not exact.
        Self::from_rusttype(font, 1.125)
    }

    pub fn from_vec(bytes: Vec<u8>) -> Font {
        let Some(font) = rusttype::Font::try_from_vec(bytes) else {
            panic!("Couldn't parse font bytes.");
        };

        Self::from_rusttype(font, 1.0) // No scale factor
    }

    pub fn mono_regular() -> Font {
        Self::from_resource_file("LiberationMono-Regular.ttf")
    }
    pub fn mono_bold() -> Font {
        Self::from_resource_file("LiberationMono-Bold.ttf")
    }
    pub fn mono_bold_italic() -> Font {
        Self::from_resource_file("LiberationMono-BoldItalic.ttf")
    }
    pub fn mono_italic() -> Font {
        Self::from_resource_file("LiberationMono-Italic.ttf")
    }
    pub fn sans_regular() -> Font {
        Self::from_resource_file("LiberationSans-Regular.ttf")
    }
    pub fn sans_bold() -> Font {
        Self::from_resource_file("LiberationSans-Bold.ttf")
    }
    pub fn sans_bold_italic() -> Font {
        Self::from_resource_file("LiberationSans-BoldItalic.ttf")
    }
    pub fn sans_italic() -> Font {
        Self::from_resource_file("LiberationSans-Italic.ttf")
    }
    pub fn serif_regular() -> Font {
        Self::from_resource_file("LiberationSerif-Regular.ttf")
    }
    pub fn serif_bold() -> Font {
        Self::from_resource_file("LiberationSerif-Bold.ttf")
    }
    pub fn serif_bold_italic() -> Font {
        Self::from_resource_file("LiberationSerif-BoldItalic.ttf")
    }
    pub fn serif_italic() -> Font {
        Self::from_resource_file("LiberationSerif-Italic.ttf")
    }
    pub fn sans_regular_ja() -> Font {
        Self::from_resource_file("NotoSansJP-Regular.otf")
    }
    pub fn sans_bold_ja() -> Font {
        Self::from_resource_file("NotoSansJP-Bold.otf")
    }

    pub fn units_per_em(&self) -> u16 {
        self.font.units_per_em()
    }

    pub fn ascent_unscaled(&self) -> f32 {
        self.font.v_metrics_unscaled().ascent
    }
    pub fn descent_unscaled(&self) -> f32 {
        self.font.v_metrics_unscaled().descent
    }
    pub fn line_gap_unscaled(&self) -> f32 {
        self.font.v_metrics_unscaled().line_gap
    }

    pub fn ascent(&self, font_size: f32) -> f32 {
        let v_metrics = self.v_metrics_scaled(font_size);
        v_metrics.ascent
    }
    pub fn descent(&self, font_size: f32) -> f32 {
        let v_metrics = self.v_metrics_scaled(font_size);
        v_metrics.descent
    }

    pub fn line_gap(&self, font_size: f32) -> f32 {
        let v_metrics = self.v_metrics_scaled(font_size);
        v_metrics.line_gap
    }

    fn as_face_ref(&self) -> &owned_ttf_parser::Face<'_> {
        match &self.font {
            rusttype::Font::Owned(f) => f.as_face_ref(),
            _ => unreachable!(),
        }
    }

    pub fn global_bounding_box(&self) -> (i16, i16, i16, i16) {
        let rect = self.as_face_ref().global_bounding_box();
        (rect.x_min, rect.y_min, rect.x_max, rect.y_max)
    }

    pub fn glyph_hor_advance(&self, glyph_id: u16) -> Option<u16> {
        self.as_face_ref()
            .glyph_hor_advance(owned_ttf_parser::GlyphId(glyph_id))
    }

    pub fn glyph_bounding_box(&self, glyph_id: u16) -> Option<(i16, i16, i16, i16)> {
        let rect = self
            .as_face_ref()
            .glyph_bounding_box(owned_ttf_parser::GlyphId(glyph_id));
        rect.map(|rect| (rect.x_min, rect.y_min, rect.x_max, rect.y_max))
    }

    pub fn italic_angle(&self) -> Option<f32> {
        self.as_face_ref().italic_angle()
    }

    pub fn table_data(&self, tag: u32) -> Option<&[u8]> {
        self.as_face_ref().table_data(owned_ttf_parser::Tag(tag))
    }

    pub fn number_of_glyphs(&self) -> u16 {
        self.as_face_ref().number_of_glyphs()
    }

    fn line_height_and_gap(&self, font_size: f32) -> (f32, f32) {
        let v_metrics = self.v_metrics_scaled(font_size);
        (v_metrics.ascent - v_metrics.descent, v_metrics.line_gap)
    }

    #[inline]
    fn pair_kerning_fast(
        &self,
        factor: f32,
        scale: Scale,
        last: (GlyphId, Option<u8>),
        cur_gid: GlyphId,
        cur_ascii: Option<u8>,
    ) -> f32 {
        if !self.has_kern {
            return 0.0;
        }
        if let (Some(a), Some(b)) = (last.1, cur_ascii) {
            let mut slot_ref = self.ascii_kern_raw.borrow_mut();
            let table = slot_ref.get_or_insert_with(|| Box::new([i32::MIN; 128 * 128]));
            let idx = (a as usize) * 128 + (b as usize);
            let cached = table[idx];
            let raw = if cached != i32::MIN {
                cached
            } else {
                let k = self
                    .as_face_ref()
                    .tables()
                    .kern
                    .and_then(|kern| {
                        kern.subtables
                            .into_iter()
                            .filter(|st| st.horizontal && !st.variable)
                            .find_map(|st| {
                                st.glyphs_kerning(
                                    owned_ttf_parser::GlyphId(last.0 .0),
                                    owned_ttf_parser::GlyphId(cur_gid.0),
                                )
                            })
                    })
                    .unwrap_or(0) as i32;
                table[idx] = k;
                k
            };
            factor * (raw as f32)
        } else {
            self.font.pair_kerning(scale, last.0, cur_gid)
        }
    }

    /// n72 fork: rusttype's `layout` without positioning each glyph. Same
    /// caret, kerning and advance maths as its LayoutIter, but `positioned()`
    /// computes the glyph's pixel bounds, which in ttf-parser walks the whole
    /// outline: that was paid for every glyph of every text drawn, every frame.
    fn layout_unpositioned<'s>(
        &'s self,
        text: &'s str,
        scale: Scale,
        start: Point<f32>,
    ) -> impl Iterator<Item = (GlyphId, f32, f32, Point<f32>)> + 's {
        let factor = if scale.x == scale.y {
            self.font.scale_for_pixel_height(scale.y)
        } else {
            self.font.scale_for_pixel_height(scale.y) * (scale.x / scale.y)
        };
        let mut caret = 0.0f32;
        let mut last: Option<(GlyphId, Option<u8>)> = None;
        text.chars().map(move |c| {
            let (gid, adv, lsb, ascii) = if (c as u32) < 128 {
                let b = c as u8;
                let gid = self.ascii_glyph_ids[b as usize];
                let (raw_adv, raw_lsb) = self.ascii_h_metrics_raw[b as usize];
                (gid, (raw_adv as f32) * factor, (raw_lsb as f32) * factor, Some(b))
            } else {
                let g = self.font.glyph(c).scaled(scale);
                let hm = g.h_metrics();
                (g.id(), hm.advance_width, hm.left_side_bearing, None)
            };
            if let Some(l) = last {
                caret += self.pair_kerning_fast(factor, scale, l, gid, ascii);
            }
            let p = Point {
                x: start.x + caret,
                y: start.y,
            };
            caret += adv;
            last = Some((gid, ascii));
            (gid, adv, lsb, p)
        })
    }

    /// Calculate the width of a line. This does not handle newlines!
    fn calculate_line_width(&self, font_size: f32, line: &str) -> f32 {
        let mut line_x_min: f32 = 0.0;
        let mut line_x_max: f32 = 0.0;

        for (_gid, advance_width, left_side_bearing, position) in
            self.layout_unpositioned(line, self.scale(font_size), Default::default())
        {
            // This method used to use pixel_bounding_box() for metrics, but
            // now uses h_metrics() in order to support whitespace characters.
            // This definition of character width was chosen because it gave
            // similar results to the old implementation, not because it's
            // optimal; maybe it could be improved.
            let glyph_x_min = position.x.min(position.x + left_side_bearing);
            let glyph_x_max = position.x + advance_width;

            line_x_min = line_x_min.min(glyph_x_min);
            line_x_min = line_x_min.min(glyph_x_max);
            line_x_max = line_x_max.max(glyph_x_min);
            line_x_max = line_x_max.max(glyph_x_max);
        }

        // This rounding is also to emulate pixel_bounding_box(), same caveat
        // applies.
        line_x_max.ceil() - line_x_min.floor()
    }

    /// Break text into lines with known widths.
    pub fn break_lines<'a>(
        &self,
        font_size: f32,
        text: &'a str,
        wrap: Option<(f32, WrapMode)>,
    ) -> Vec<(f32, &'a str)> {
        let mut lines = Vec::new();

        for line in text.lines() {
            let Some((wrap_width, wrap_mode)) = wrap else {
                lines.push((self.calculate_line_width(font_size, line), line));
                continue;
            };

            let unwrapped_line = line;

            // Find points at which the line could be wrapped
            let mut wrap_points = Vec::new();
            match wrap_mode {
                WrapMode::Word => {
                    let mut word_start = 0;

                    while let Some(i) = line[word_start..].find(|c: char| c.is_whitespace()) {
                        let word_end = word_start + i;
                        // Include any additional whitespace in the word,
                        // so that the next word begins with non-whitespace.
                        let Some(i) = line[word_end..].find(|c: char| !c.is_whitespace()) else {
                            break;
                        };
                        wrap_points.push(word_end + i);
                        word_start = word_end + i;
                    }
                }
                WrapMode::Char => {
                    let mut char_end = 1;
                    while char_end < line.len() {
                        if line.is_char_boundary(char_end) {
                            wrap_points.push(char_end);
                        }
                        char_end += 1;
                    }
                }
            };
            wrap_points.push(line.len());

            let mut next_wrap_point_idx = 0;
            let mut line_start = 0;

            fn trim_wrapped_line(wrap_mode: WrapMode, line: &str) -> &str {
                // Spaces before a word wrap point are ignored for
                // wrapping purposes.
                if wrap_mode == WrapMode::Word {
                    line.trim_end()
                } else {
                    line
                }
            }

            while next_wrap_point_idx < wrap_points.len() {
                // Find optimal line wrapping by binary search.
                // `binary_search_by` returns Err when there's no exactly
                // matching line length, which is usually going to be the case.
                let wrap_search_result =
                    wrap_points[next_wrap_point_idx..].binary_search_by(|&wrap_point| {
                        let line = &line[line_start..wrap_point];
                        let line_width = self
                            .calculate_line_width(font_size, trim_wrapped_line(wrap_mode, line));
                        line_width.partial_cmp(&wrap_width).unwrap()
                    });
                let wrap_point_idx = match wrap_search_result {
                    Ok(i) => next_wrap_point_idx + i,
                    Err(i @ 1..) => next_wrap_point_idx + (i - 1),
                    _ => {
                        // The span between the current wrap point and the next
                        // wrap point is wider than the wrap width. In practice,
                        // this means a word too big to fit on-screen.
                        if matches!(wrap_mode, WrapMode::Word) {
                            // Try to break the word.
                            let word_end = wrap_points[next_wrap_point_idx];
                            let word = &line[line_start..word_end];
                            let broken_words = self.break_lines(
                                font_size,
                                word,
                                Some((wrap_width, WrapMode::Char)),
                            );
                            lines.extend(broken_words);
                            next_wrap_point_idx += 1;
                            line_start = word_end;
                            continue;
                        }
                        // It can't be helped: truncate.
                        next_wrap_point_idx
                    }
                };
                let line_end = wrap_points[wrap_point_idx];
                let line = &line[line_start..line_end];

                let trimmed_line = if line_end != unwrapped_line.len() {
                    // Whitespace at the end of a line must only be ignored if
                    // that line break came from word wrapping.
                    trim_wrapped_line(wrap_mode, line)
                } else {
                    line
                };

                lines.push((
                    self.calculate_line_width(font_size, trimmed_line),
                    trimmed_line,
                ));

                next_wrap_point_idx = wrap_point_idx + 1;
                line_start = line_end;
            }
        }

        lines
    }

    fn text_size_for_lines(lines: &[(f32, &str)], line_height: f32, line_gap: f32) -> (f32, f32) {
        let width = lines
            .iter()
            .fold(0f32, |widest, &(line_width, _line)| widest.max(line_width));
        let height =
            line_height * (lines.len() as f32) + line_gap * (lines.len().saturating_sub(1) as f32);
        (width, height)
    }

    /// Calculate the on-screen width and height of text with a given font size.
    pub fn calculate_text_size(
        &self,
        font_size: f32,
        text: &str,
        wrap: Option<(f32, WrapMode)>,
    ) -> (f32, f32) {
        let lines = self.break_lines(font_size, text, wrap);
        let (line_height, line_gap) = self.line_height_and_gap(font_size);
        Self::text_size_for_lines(&lines, line_height, line_gap)
    }

    /// Draw text and return its `(width, height)`. Calls the provided callback
    /// for each glyph that is to be drawn. Assumes y starts at the top-left
    /// corner and points downwards. Used by UIKit for font rendering.
    pub fn draw<F: FnMut(RasterGlyph)>(
        &self,
        font_size: f32,
        text: &str,
        origin: (f32, f32),
        wrap: Option<(f32, WrapMode)>,
        alignment: TextAlignment,
        mut draw_glyph: F,
    ) -> (f32, f32) {
        let lines = self.break_lines(font_size, text, wrap);

        let scale = self.scale(font_size);
        let scale_bits = scale.x.to_bits();
        let mut baseline = origin.1 + self.font.v_metrics(scale).ascent;
        let (line_height, line_gap) = self.line_height_and_gap(font_size);
        let text_size = Self::text_size_for_lines(&lines, line_height, line_gap);

        let mut cache = self.glyph_cache.borrow_mut();

        for (line_width, line_text) in lines {
            let line_x_offset = match alignment {
                TextAlignment::Left => 0.0,
                TextAlignment::Center => -line_width / 2.0,
                TextAlignment::Right => -line_width,
            };
            for (gid, _adv, _lsb, pos) in self.layout_unpositioned(
                line_text,
                scale,
                Point {
                    x: origin.0 + line_x_offset,
                    y: baseline,
                },
            ) {
                // n72 fork: snap the pen to quarter pixels and reuse the
                // rasterized glyph for that (glyph, size, sub-position).
                let (ix, iy) = (pos.x.floor(), pos.y.floor());
                let qx = ((pos.x - ix) * 4.0).round() as u8; // 0..=4
                let qy = ((pos.y - iy) * 4.0).round() as u8;
                let key: GlyphKey = (gid.0, scale_bits, qx, qy);
                if cache.map.len() >= GlyphCache::MAX_ENTRIES {
                    cache.map.clear();
                }
                let cached = cache.map.entry(key).or_insert_with(|| {
                    let snapped = self.font.glyph(gid).scaled(scale).positioned(Point {
                        x: qx as f32 / 4.0,
                        y: qy as f32 / 4.0,
                    });
                    let bounds = snapped.pixel_bounding_box()?;
                    let dims = (bounds.width() as usize, bounds.height() as usize);
                    let mut bitmap = vec![0.0f32; dims.0 * dims.1];
                    snapped.draw(|x, y, coverage| {
                        bitmap[y as usize * dims.0 + x as usize] = coverage;
                    });
                    Some(((bounds.min.x, bounds.min.y), dims, bitmap))
                });
                let Some(((bx, by), dims, bitmap)) = cached else {
                    continue;
                };
                let glyph_origin = (ix as i32 + *bx, iy as i32 + *by);

                // TODO: Refactor this method to support y clipping too.
                // It's not mandatory since the caller can do it, but it would
                // be more efficient.
                if let Some((wrap_width, _)) = wrap {
                    if glyph_origin.0 as f32 > origin.0 + wrap_width {
                        // Avoid wasting effort on glyphs that are entirely
                        // clipped. Partial clipping is the responsibility of
                        // the draw_glyph implementation.
                        continue;
                    }
                }

                let raster_glyph = RasterGlyph {
                    origin: (glyph_origin.0 as f32, glyph_origin.1 as f32),
                    dimensions: (dims.0 as i32, dims.1 as i32),
                    pixels: bitmap,
                };

                draw_glyph(raster_glyph);
            }
            baseline += line_height + line_gap;
        }
        text_size
    }

    /// Draw glyphs. Similar to [Self::draw], but uses raw glyph ids instead of
    /// text and doesn't account for line breaks or text alignment (those
    /// should be handled by the caller). Used by CoreGraphics for font
    /// rendering.
    /// TODO: unify with [Self::draw]. Note: y sense is different! If you
    /// plan to do that refactoring, make sure that there are no visual
    /// regressions in the GUI tests for CGFont/CGGlyph of the TestApp!
    pub fn draw_glyphs<I, F>(
        &self,
        font_size: f32,
        glyphs: I,
        origin: (f32, f32),
        text_transform: CGAffineTransform,
        mut draw_glyph: F,
    ) where
        I: IntoIterator<Item = GlyphId>,
        F: FnMut(RasterGlyph),
    {
        // TODO: avoid creating a tmp bitmap
        let mut tmp_glyph_bitmap: Vec<f32> = Vec::new();
        // Cf. comment in the [Self::draw] function.
        let mut glyph_bitmap: Vec<f32> = Vec::new();

        let inverted_text_transform = text_transform.invert();

        // rusttype only supports scaling, so we render each glyph at a
        // resolution matched to the effective scale of the text
        // transform and then resample to apply rotation/mirror/etc.
        // TODO: apply x and y scales independently?
        let unit_x = text_transform.apply_to_size(CGSize {
            width: 1.0,
            height: 0.0,
        });
        let unit_y = text_transform.apply_to_size(CGSize {
            width: 0.0,
            height: 1.0,
        });
        let tmp_font_scale = unit_x
            .width
            .hypot(unit_x.height)
            .max(unit_y.width.hypot(unit_y.height))
            .max(1.0);

        let start = Point { x: 0.0, y: 0.0 };
        // This code is adapted from documentation of [rusttype::Font::layout].
        let iter = self
            .font
            .glyphs_for(glyphs.into_iter())
            .scan((None, 0.0), |(last, x), g| {
                let g = g.scaled(self.scale(font_size * tmp_font_scale));
                if let Some(last) = last {
                    *x += self.font.pair_kerning(
                        self.scale(font_size * tmp_font_scale),
                        *last,
                        g.id(),
                    );
                }
                let w = g.h_metrics().advance_width;
                let next = g.positioned(start + vector(*x, 0.0));
                *last = Some(next.id());
                *x += w;
                Some(next)
            });
        for glyph in iter {
            let Some(glyph_bounds) = glyph.pixel_bounding_box() else {
                continue;
            };
            let rect = CGRect {
                origin: CGPoint {
                    x: glyph_bounds.min.x as f32 / tmp_font_scale,
                    // Note: this is because y is inverted!
                    y: -glyph_bounds.max.y as f32 / tmp_font_scale,
                },
                size: CGSize {
                    width: glyph_bounds.width() as f32 / tmp_font_scale,
                    height: glyph_bounds.height() as f32 / tmp_font_scale,
                },
            };

            let transformed = text_transform.apply_to_rect(rect);
            assert!(rect.size.width >= 0.0 && rect.size.height >= 0.0);
            log_dbg!(
                "draw_glyphs: glyph {:?}, bounds {:?}, rect {:?}, transformed {:?}",
                glyph,
                glyph_bounds,
                rect,
                transformed
            );

            let x_offset = (origin.0 + transformed.origin.x).round() as i32;
            let y_offset = (origin.1 + transformed.origin.y).round() as i32;

            let tmp_glyph_bitmap_bounds = (
                glyph_bounds.width() as usize,
                glyph_bounds.height() as usize,
            );
            log_dbg!("tmp_glyph_bitmap_bounds {:?}", tmp_glyph_bitmap_bounds);
            tmp_glyph_bitmap.clear();
            tmp_glyph_bitmap.resize(tmp_glyph_bitmap_bounds.0 * tmp_glyph_bitmap_bounds.1, 0.0);

            glyph.draw(|x, y, coverage| {
                // Note: need to fill the bitmap in the reverse y order to
                // account for y sense
                tmp_glyph_bitmap[(tmp_glyph_bitmap_bounds.1 - 1 - y as usize)
                    * tmp_glyph_bitmap_bounds.0
                    + x as usize] = coverage;
            });

            // Ceil so fractional pixels at the right/bottom edge aren't
            // clipped.
            let glyph_bitmap_bounds = (
                transformed.size.width.ceil() as usize,
                transformed.size.height.ceil() as usize,
            );
            log_dbg!(
                "x_offset {}, y_offset {}, size {:?}",
                x_offset,
                y_offset,
                glyph_bitmap_bounds
            );
            glyph_bitmap.clear();
            glyph_bitmap.resize(glyph_bitmap_bounds.0 * glyph_bitmap_bounds.1, 0.0);

            for y in 0..glyph_bitmap_bounds.1 {
                for x in 0..glyph_bitmap_bounds.0 {
                    let point = CGPoint {
                        x: transformed.origin.x + x as f32 + 0.5,
                        y: transformed.origin.y + y as f32 + 0.5,
                    };
                    let orig = inverted_text_transform.apply_to_point(point);
                    let orig_x = (orig.x * tmp_font_scale - glyph_bounds.min.x as f32) as i32;
                    let orig_y = (orig.y * tmp_font_scale + glyph_bounds.max.y as f32) as i32;
                    log_dbg!("({}, {}) -> ({}, {})", x, y, orig_x, orig_y);
                    if orig_x >= 0
                        && orig_y >= 0
                        && orig_x < tmp_glyph_bitmap_bounds.0 as i32
                        && orig_y < tmp_glyph_bitmap_bounds.1 as i32
                    {
                        let idx_orig =
                            (orig_y * tmp_glyph_bitmap_bounds.0 as i32 + orig_x) as usize;
                        glyph_bitmap[y * glyph_bitmap_bounds.0 + x] = tmp_glyph_bitmap[idx_orig];
                    }
                }
            }

            let raster_glyph = RasterGlyph {
                origin: (x_offset as f32, y_offset as f32),
                dimensions: (glyph_bitmap_bounds.0 as _, glyph_bitmap_bounds.1 as _),
                pixels: &glyph_bitmap,
            };

            draw_glyph(raster_glyph);
        }
    }
}

#[cfg(test)]
#[test]
fn test_ascii_layout_and_size_match_rusttype() {
    let font = Font::sans_regular();
    let samples = [
        "AVATAR To Wa 0123456789.,!?:;-+()[] ",
        "Watermelon Topaz AVA",
        "Café naïve — € 100%",
        "Line one wraps nicely across multiple words for testing To Wa AV.",
    ];
    for &size in &[10.0f32, 14.0, 18.5] {
        let scale = font.scale(size);
        for &text in &samples {
            // 1. Direct rusttype layout must match layout_unpositioned bit-for-bit.
            let mut rt_caret = 0.0f32;
            let mut rt_last: Option<GlyphId> = None;
            let expected: Vec<(GlyphId, f32, f32, f32)> = text
                .chars()
                .map(|c| {
                    let g = font.font.glyph(c).scaled(scale);
                    if let Some(l) = rt_last {
                        rt_caret += font.font.pair_kerning(scale, l, g.id());
                    }
                    let hm = g.h_metrics();
                    let px = 3.25 + rt_caret;
                    rt_caret += hm.advance_width;
                    rt_last = Some(g.id());
                    (g.id(), hm.advance_width, hm.left_side_bearing, px)
                })
                .collect();

            let actual: Vec<(GlyphId, f32, f32, f32)> = font
                .layout_unpositioned(text, scale, Point { x: 3.25, y: 10.0 })
                .map(|(gid, adv, lsb, pos)| (gid, adv, lsb, pos.x))
                .collect();
            assert_eq!(actual.len(), expected.len());
            for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
                assert_eq!(a.0, e.0, "gid mismatch at {i} for {text:?} size {size}");
                assert!(
                    (a.1 - e.1).abs() <= 1e-6 && (a.2 - e.2).abs() <= 1e-6 && (a.3 - e.3).abs() <= 1e-5,
                    "metric/pos mismatch at {i} for {text:?} size {size}: {a:?} vs {e:?}"
                );
            }

            // 2. draw() must return exact calculate_text_size() and produce
            // consistent glyph origins/dimensions across repeated calls (cache hit).
            for wrap in [None, Some((120.0, WrapMode::Word))] {
                let sz_calc = font.calculate_text_size(size, text, wrap);
                let mut g1 = Vec::new();
                let sz_draw1 = font.draw(size, text, (5.5, 8.0), wrap, TextAlignment::Center, |rg| {
                    g1.push((rg.origin(), rg.dimensions(), rg.pixels().len()));
                });
                let mut g2 = Vec::new();
                let sz_draw2 = font.draw(size, text, (5.5, 8.0), wrap, TextAlignment::Center, |rg| {
                    g2.push((rg.origin(), rg.dimensions(), rg.pixels().len()));
                });
                assert_eq!(sz_draw1, sz_calc);
                assert_eq!(sz_draw2, sz_calc);
                assert_eq!(g1, g2);
            }
        }
    }
}
