/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CGBitmapContext.h`

use super::cg_affine_transform::{CGAffineTransform, CGAffineTransformIdentity};
use super::cg_color_space::{
    kCGColorSpaceGenericGray, kCGColorSpaceGenericRGB, CGColorSpaceHostObject, CGColorSpaceRef,
};
use super::cg_context::{
    kCGBlendModeCopy, kCGBlendModeDarken, kCGBlendModeLighten, kCGBlendModeMultiply,
    kCGBlendModeNormal, kCGBlendModeScreen, CGBlendMode, CGContextHostObject, CGContextRef,
    CGContextSubclass,
};
use super::cg_image::{
    self, kCGBitmapAlphaInfoMask, kCGBitmapByteOrderMask, kCGImageAlphaFirst, kCGImageAlphaLast,
    kCGImageAlphaNone, kCGImageAlphaNoneSkipFirst, kCGImageAlphaNoneSkipLast, kCGImageAlphaOnly,
    kCGImageAlphaPremultipliedFirst, kCGImageAlphaPremultipliedLast, kCGImageByteOrder32Big,
    kCGImageByteOrderDefault, CGBitmapInfo, CGImageAlphaInfo, CGImageRef,
};
use super::{CGFloat, CGPoint, CGRect};
use crate::dyld::{export_c_func, FunctionExports};
use crate::image::{gamma_decode, gamma_decode_u8, gamma_encode_fast, Image};
use crate::mem::{GuestUSize, Mem, MutVoidPtr, Ptr};
use crate::objc::ObjC;
use crate::Environment;

#[derive(Copy, Clone)]
pub(super) struct CGBitmapContextData {
    pub(super) data: MutVoidPtr,
    pub(super) data_is_owned: bool,
    width: GuestUSize,
    height: GuestUSize,
    bits_per_component: GuestUSize,
    bytes_per_row: GuestUSize,
    color_space: &'static str,
    alpha_info: CGImageAlphaInfo,
}

pub fn CGBitmapContextCreate(
    env: &mut Environment,
    data: MutVoidPtr,
    width: GuestUSize,
    height: GuestUSize,
    bits_per_component: GuestUSize,
    bytes_per_row: GuestUSize,
    color_space: CGColorSpaceRef,
    bitmap_info: u32,
) -> CGContextRef {
    assert!(bits_per_component == 8); // TODO: support other bit depths

    let color_space = env.objc.borrow::<CGColorSpaceHostObject>(color_space).name;

    let component_count = match color_space {
        kCGColorSpaceGenericRGB => components_for_rgb(bitmap_info).unwrap(),
        kCGColorSpaceGenericGray => components_for_gray(bitmap_info).unwrap(),
        _ => unimplemented!("support other color spaces"),
    };

    let (data, data_is_owned, bytes_per_row) = if data.is_null() {
        let bytes_per_row = if bytes_per_row == 0 {
            width.checked_mul(component_count).unwrap()
        } else {
            bytes_per_row
        };
        let total_size = bytes_per_row.checked_mul(height).unwrap();
        let data = env.mem.alloc(total_size);
        (data, true, bytes_per_row)
    } else {
        assert!(bytes_per_row != 0);
        (data, false, bytes_per_row)
    };

    let host_object = CGContextHostObject {
        subclass: CGContextSubclass::CGBitmapContext(CGBitmapContextData {
            data,
            data_is_owned,
            width,
            height,
            bits_per_component,
            bytes_per_row,
            color_space,
            alpha_info: bitmap_info & kCGBitmapAlphaInfoMask,
        }),
        // TODO: is this the correct default?
        rgb_fill_color: (0.0, 0.0, 0.0, 0.0),
        font: Ptr::null(),
        font_size: 14.0,
        transform: CGAffineTransformIdentity,
        blend_mode: kCGBlendModeNormal,
        text_transform: None,
        state_stack: Vec::new(),
    };
    let isa = env
        .objc
        .get_known_class("_touchHLE_CGContext", &mut env.mem);
    env.objc
        .alloc_object(isa, Box::new(host_object), &mut env.mem)
}

fn CGBitmapContextGetData(env: &mut Environment, context: CGContextRef) -> MutVoidPtr {
    let host_obj = env.objc.borrow::<CGContextHostObject>(context);
    let CGContextSubclass::CGBitmapContext(bitmap_data) = host_obj.subclass;
    bitmap_data.data
}

pub fn CGBitmapContextGetWidth(env: &mut Environment, context: CGContextRef) -> GuestUSize {
    let host_obj = env.objc.borrow::<CGContextHostObject>(context);
    let CGContextSubclass::CGBitmapContext(bitmap_data) = host_obj.subclass;
    bitmap_data.width
}

pub fn CGBitmapContextGetHeight(env: &mut Environment, context: CGContextRef) -> GuestUSize {
    let host_obj = env.objc.borrow::<CGContextHostObject>(context);
    let CGContextSubclass::CGBitmapContext(bitmap_data) = host_obj.subclass;
    bitmap_data.height
}

fn CGBitmapContextGetBytesPerRow(env: &mut Environment, context: CGContextRef) -> GuestUSize {
    let host_obj = env.objc.borrow::<CGContextHostObject>(context);
    let CGContextSubclass::CGBitmapContext(bitmap_data) = host_obj.subclass;
    bitmap_data.bytes_per_row
}

pub fn CGBitmapContextCreateImage(env: &mut Environment, context: CGContextRef) -> CGImageRef {
    // TODO: Image::from_pixel_vec() should not exist, and this function should
    // support any bitmap format.
    let host_obj = env.objc.borrow::<CGContextHostObject>(context);
    let CGContextSubclass::CGBitmapContext(bitmap_data) = host_obj.subclass;
    assert!(
        bitmap_data.bits_per_component == 8
            && bitmap_data.bytes_per_row == bitmap_data.width * 4
            && bitmap_data.color_space == kCGColorSpaceGenericRGB
            && matches!(
                bitmap_data.alpha_info,
                kCGImageAlphaNoneSkipLast | kCGImageAlphaPremultipliedLast
            )
    );
    let pixels = env
        .mem
        .bytes_at(
            bitmap_data.data.cast(),
            bitmap_data.bytes_per_row * bitmap_data.height,
        )
        .to_vec();
    cg_image::from_image(
        env,
        Image::from_pixel_vec(pixels, (bitmap_data.width, bitmap_data.height)),
    )
}

fn components_for_rgb(bitmap_info: CGBitmapInfo) -> Result<GuestUSize, ()> {
    let byte_order = bitmap_info & kCGBitmapByteOrderMask;
    if byte_order != kCGImageByteOrderDefault && byte_order != kCGImageByteOrder32Big {
        return Err(()); // TODO: handle other byte orders
    }

    let alpha_info = bitmap_info & kCGBitmapAlphaInfoMask;
    if (alpha_info | byte_order) != bitmap_info {
        return Err(()); // TODO: handle other cases (float)
    }
    match alpha_info & kCGBitmapAlphaInfoMask {
        kCGImageAlphaNone => Ok(3), // RGB
        kCGImageAlphaPremultipliedLast
        | kCGImageAlphaPremultipliedFirst
        | kCGImageAlphaLast
        | kCGImageAlphaFirst
        | kCGImageAlphaNoneSkipLast
        | kCGImageAlphaNoneSkipFirst => Ok(4), // RGBA/ARGB/RGBX/XRGB
        kCGImageAlphaOnly => Ok(1), // A
        _ => Err(()),               // unknown values
    }
}

fn components_for_gray(bitmap_info: CGBitmapInfo) -> Result<GuestUSize, ()> {
    let byte_order = bitmap_info & kCGBitmapByteOrderMask;
    if byte_order != kCGImageByteOrderDefault && byte_order != kCGImageByteOrder32Big {
        return Err(()); // TODO: handle other byte orders
    }

    let alpha_info = bitmap_info & kCGBitmapAlphaInfoMask;
    if (alpha_info | byte_order) != bitmap_info {
        return Err(()); // TODO: handle other cases (float)
    }
    match alpha_info & kCGBitmapAlphaInfoMask {
        kCGImageAlphaNone => Ok(1), // gray
        kCGImageAlphaPremultipliedLast
        | kCGImageAlphaPremultipliedFirst
        | kCGImageAlphaLast
        | kCGImageAlphaFirst
        | kCGImageAlphaNoneSkipLast
        | kCGImageAlphaNoneSkipFirst => Ok(2), // gray + alpha
        kCGImageAlphaOnly => Ok(1), // A
        _ => Err(()),               // unknown values
    }
}

fn bytes_per_pixel(data: &CGBitmapContextData) -> GuestUSize {
    let &CGBitmapContextData {
        bits_per_component,
        color_space,
        alpha_info,
        ..
    } = data;
    assert!(bits_per_component == 8);
    match color_space {
        kCGColorSpaceGenericRGB => components_for_rgb(alpha_info).unwrap(),
        kCGColorSpaceGenericGray => components_for_gray(alpha_info).unwrap(),
        _ => unimplemented!("support other color spaces"),
    }
}

fn get_pixels<'a>(data: &CGBitmapContextData, mem: &'a mut Mem) -> &'a mut [u8] {
    let pixel_data_size = data.height.checked_mul(data.bytes_per_row).unwrap();
    mem.bytes_at_mut(data.data.cast(), pixel_data_size)
}

/// Blends two RGBA non gamma-encoded values, with straight alpha,
/// using a blend mode.
fn blend_straight(
    bg: (f32, f32, f32, f32),
    fg: (f32, f32, f32, f32),
    blend_mode: CGBlendMode,
) -> (f32, f32, f32, f32) {
    assert_eq!(blend_mode, kCGBlendModeNormal); // TODO
    if fg.3 == 0.0 {
        // If fg.3 == 0.0 we attempt to blend fully transparent color.
        bg
    } else {
        let neg_fg_a = 1.0 - fg.3;
        let new_a = fg.3 + bg.3 * neg_fg_a; // Can't be 0 if fg.3 != 0
        (
            (fg.0 * fg.3 + bg.0 * bg.3 * neg_fg_a) / new_a,
            (fg.1 * fg.3 + bg.1 * bg.3 * neg_fg_a) / new_a,
            (fg.2 * fg.3 + bg.2 * bg.3 * neg_fg_a) / new_a,
            new_a,
        )
    }
}

/// Blends two RGBA non gamma-encoded values, with premultiplied alpha,
/// using a blend mode.
/// See [Blending](https://www.w3.org/TR/compositing-1/#blending) for details.
fn blend_premultiplied(
    bg: (f32, f32, f32, f32),
    fg: (f32, f32, f32, f32),
    blend_mode: CGBlendMode,
) -> (f32, f32, f32, f32) {
    if blend_mode == kCGBlendModeCopy {
        return fg;
    }
    // Blend
    let blend_res = match blend_mode {
        kCGBlendModeNormal => (bg.3 * fg.0, bg.3 * fg.1, bg.3 * fg.2),
        kCGBlendModeMultiply => (bg.0 * fg.0, bg.1 * fg.1, bg.2 * fg.2),
        kCGBlendModeScreen => (
            fg.3 * bg.0 + bg.3 * fg.0 - bg.0 * fg.0,
            fg.3 * bg.1 + bg.3 * fg.1 - bg.1 * fg.1,
            fg.3 * bg.2 + bg.3 * fg.2 - bg.2 * fg.2,
        ),
        kCGBlendModeDarken => (
            (fg.3 * bg.0).min(bg.3 * fg.0),
            (fg.3 * bg.1).min(bg.3 * fg.1),
            (fg.3 * bg.2).min(bg.3 * fg.2),
        ),
        kCGBlendModeLighten => (
            (fg.3 * bg.0).max(bg.3 * fg.0),
            (fg.3 * bg.1).max(bg.3 * fg.1),
            (fg.3 * bg.2).max(bg.3 * fg.2),
        ),
        _ => unimplemented!("blend mode {}", blend_mode),
    };
    // Compose
    let neg_bg_a = 1.0 - bg.3;
    let neg_fg_a = 1.0 - fg.3;
    (
        fg.0 * neg_bg_a + blend_res.0 + bg.0 * neg_fg_a,
        fg.1 * neg_bg_a + blend_res.1 + bg.1 * neg_fg_a,
        fg.2 * neg_bg_a + blend_res.2 + bg.2 * neg_fg_a,
        fg.3 + bg.3 * neg_fg_a,
    )
}

/// per component offsets (r, g, b, a)
fn pixel_offsets(data: &CGBitmapContextData) -> (usize, usize, usize, Option<usize>) {
    match data.color_space {
        kCGColorSpaceGenericRGB => {
            match data.alpha_info {
                kCGImageAlphaNone => (0, 1, 2, None),
                kCGImageAlphaPremultipliedLast | kCGImageAlphaLast => (0, 1, 2, Some(3)),
                kCGImageAlphaPremultipliedFirst | kCGImageAlphaFirst => (1, 2, 3, Some(0)),
                kCGImageAlphaNoneSkipLast => (0, 1, 2, None),
                kCGImageAlphaNoneSkipFirst => (1, 2, 3, None),
                kCGImageAlphaOnly => (0, 0, 0, Some(0)),
                _ => unreachable!(), // checked by bytes_per_pixel
            }
        }
        kCGColorSpaceGenericGray => {
            // TODO: this is probably isn't doing RGB to grayscale conversion
            // properly
            match data.alpha_info {
                kCGImageAlphaNone => (0, 0, 0, None),
                kCGImageAlphaPremultipliedLast | kCGImageAlphaLast => (0, 0, 0, Some(1)),
                kCGImageAlphaPremultipliedFirst | kCGImageAlphaFirst => (1, 1, 1, Some(0)),
                kCGImageAlphaNoneSkipLast => (0, 0, 0, None),
                kCGImageAlphaNoneSkipFirst => (1, 1, 1, None),
                kCGImageAlphaOnly => (0, 0, 0, Some(0)),
                _ => unreachable!(), // checked by bytes_per_pixel
            }
        }
        _ => unimplemented!(),
    }
}

/// Get gamma-decoded RGBA value.
/// (Takes the component offsets from [pixel_offsets], computed once per pixel.)
fn get_pixel_at(
    pixel_offset: (usize, usize, usize, Option<usize>),
    pixels: &[u8],
    first_component_idx: usize,
) -> (f32, f32, f32, f32) {
    (
        gamma_decode_u8(pixels[first_component_idx + pixel_offset.0]),
        gamma_decode_u8(pixels[first_component_idx + pixel_offset.1]),
        gamma_decode_u8(pixels[first_component_idx + pixel_offset.2]),
        if let Some(alpha_offest) = pixel_offset.3 {
            pixels[first_component_idx + alpha_offest] as f32 / 255.0
        } else {
            1.0
        },
    )
}

fn put_pixel(
    data: &CGBitmapContextData,
    pixel_offset: (usize, usize, usize, Option<usize>),
    pixel_size: GuestUSize,
    pixels: &mut [u8],
    coords: (i32, i32),
    pixel: (CGFloat, CGFloat, CGFloat, CGFloat),
    blend: bool,
    blend_mode: CGBlendMode,
) {
    let (x, y) = coords;
    if x < 0 || y < 0 {
        return;
    }
    let (x, y) = (x as GuestUSize, y as GuestUSize);
    if x >= data.width || y >= data.height {
        return;
    }

    // CG's co-ordinate system puts the origin in the bottom-left corner, but it
    // *seems* like the rows are nonetheless in top-to-bottom order?
    let y = data.height - 1 - y;

    let first_component_idx = (y * data.bytes_per_row + x * pixel_size) as usize;

    let bg_pixel = get_pixel_at(pixel_offset, pixels, first_component_idx);

    // Blending like this must be done in linear RGB, so this must come before
    // gamma encoding.
    let (r, g, b, a) = if blend {
        match data.alpha_info {
            kCGImageAlphaLast | kCGImageAlphaFirst => blend_straight(bg_pixel, pixel, blend_mode),
            kCGImageAlphaPremultipliedLast | kCGImageAlphaPremultipliedFirst => {
                blend_premultiplied(bg_pixel, pixel, blend_mode)
            }
            kCGImageAlphaOnly => (
                pixel.0,
                pixel.1,
                pixel.2,
                pixel.3 + bg_pixel.3 * (1.0 - pixel.3),
            ),
            _ => {
                assert_eq!(blend_mode, kCGBlendModeNormal); // TODO
                pixel
            }
        }
    } else {
        pixel
    };

    // Alpha is always linear.
    let (r, g, b) = (
        gamma_encode_fast(r),
        gamma_encode_fast(g),
        gamma_encode_fast(b),
    );
    match data.alpha_info {
        kCGImageAlphaOnly => {
            pixels[first_component_idx] = (a * 255.0) as u8;
        }
        _ => {
            pixels[first_component_idx + pixel_offset.0] = (r * 255.0) as u8;
            pixels[first_component_idx + pixel_offset.1] = (g * 255.0) as u8;
            pixels[first_component_idx + pixel_offset.2] = (b * 255.0) as u8;
            if let Some(alpha_offset) = pixel_offset.3 {
                pixels[first_component_idx + alpha_offset] = (a * 255.0) as u8;
            }
        }
    }
}

/// Abstract interface for use by host code that wants to draw in a bitmap
/// context.
pub struct CGBitmapContextDrawer<'a> {
    bitmap_info: CGBitmapContextData,
    /// [pixel_offsets] and [bytes_per_pixel] of `bitmap_info`, computed once
    /// (both compare colour-space strings).
    pixel_offset: (usize, usize, usize, Option<usize>),
    pixel_size: GuestUSize,
    rgb_fill_color: (CGFloat, CGFloat, CGFloat, CGFloat),
    blend_mode: CGBlendMode,
    transform: CGAffineTransform,
    pixels: &'a mut [u8],
}
impl CGBitmapContextDrawer<'_> {
    pub fn new<'a>(
        objc: &ObjC,
        mem: &'a mut Mem,
        context: CGContextRef,
    ) -> CGBitmapContextDrawer<'a> {
        let &CGContextHostObject {
            subclass: CGContextSubclass::CGBitmapContext(bitmap_info),
            rgb_fill_color,
            transform,
            blend_mode,
            ..
        } = objc.borrow(context);

        let pixels = get_pixels(&bitmap_info, mem);

        CGBitmapContextDrawer {
            pixel_offset: pixel_offsets(&bitmap_info),
            pixel_size: bytes_per_pixel(&bitmap_info),
            bitmap_info,
            rgb_fill_color,
            blend_mode,
            transform,
            pixels,
        }
    }

    pub fn width(&self) -> GuestUSize {
        self.bitmap_info.width
    }
    pub fn height(&self) -> GuestUSize {
        self.bitmap_info.height
    }
    /// Get the current fill color. The returned color is linear RGB, not sRGB.
    /// It has premultiplied alpha if the context does.
    pub fn rgb_fill_color(&self) -> (CGFloat, CGFloat, CGFloat, CGFloat) {
        let multiply_by = match self.bitmap_info.alpha_info {
            kCGImageAlphaPremultipliedLast | kCGImageAlphaPremultipliedFirst => {
                self.rgb_fill_color.3
            }
            _ => 1.0,
        };
        // Multiplying before decoding matches the Simulator's output.
        (
            gamma_decode(self.rgb_fill_color.0 * multiply_by),
            gamma_decode(self.rgb_fill_color.1 * multiply_by),
            gamma_decode(self.rgb_fill_color.2 * multiply_by),
            self.rgb_fill_color.3, // alpha is always linear
        )
    }
    /// n72 fork: whether drawing a fully transparent colour with blending
    /// leaves a pixel as it is: the normal blend mode, into a context that has
    /// alpha (put_pixel writes the colour unblended into one without).
    pub fn blends_normally(&self) -> bool {
        self.blend_mode == kCGBlendModeNormal
            && matches!(
                self.bitmap_info.alpha_info,
                kCGImageAlphaLast
                    | kCGImageAlphaFirst
                    | kCGImageAlphaPremultipliedLast
                    | kCGImageAlphaPremultipliedFirst
                    | kCGImageAlphaOnly
            )
    }
    /// Set the pixel at `coords` to `color`. `color` must be linear RGB, not
    /// sRGB! Note that `coords` are absolute: you must do transformation
    /// yourself.
    pub fn put_pixel(
        &mut self,
        coords: (i32, i32),
        color: (CGFloat, CGFloat, CGFloat, CGFloat),
        blend: bool,
    ) {
        n72_cg_stats::count(1);
        put_pixel(
            &self.bitmap_info,
            self.pixel_offset,
            self.pixel_size,
            self.pixels,
            coords,
            color,
            blend,
            self.blend_mode,
        )
    }

    /// n72 fork: blit an axis-aligned (`|a| == 1`, `|d| == 1`, `b == c == 0`)
    /// glyph under normal blending directly into an alpha bitmap context,
    /// avoiding per-pixel iterator/matrix/roundf work and transparent-bg reads.
    /// Returns `false` if the caller should fall back to the generic loop.
    pub fn draw_glyph_axis_aligned(
        &mut self,
        glyph_rect: CGRect,
        raster_dims: (i32, i32),
        raster_pixels: &[f32],
        fill_color: (CGFloat, CGFloat, CGFloat, CGFloat),
    ) -> bool {
        let t = self.transform;
        if t.b != 0.0 || t.c != 0.0 || t.a.abs() != 1.0 || t.d.abs() != 1.0 {
            return false;
        }
        if self.blend_mode != kCGBlendModeNormal || self.bitmap_info.bits_per_component != 8 {
            return false;
        }
        if glyph_rect.size.width <= 0.0 || glyph_rect.size.height <= 0.0 {
            return false;
        }
        let alpha_info = self.bitmap_info.alpha_info;
        let is_alpha_only = alpha_info == kCGImageAlphaOnly;
        let is_premul = matches!(
            alpha_info,
            kCGImageAlphaPremultipliedLast | kCGImageAlphaPremultipliedFirst
        );
        let is_straight = matches!(alpha_info, kCGImageAlphaLast | kCGImageAlphaFirst);
        if !is_alpha_only && !is_premul && !is_straight {
            return false;
        }
        if is_alpha_only {
            if self.pixel_size != 1 {
                return false;
            }
        } else if self.bitmap_info.color_space != kCGColorSpaceGenericRGB || self.pixel_size != 4 {
            return false;
        }
        if raster_dims.0 <= 0 || raster_dims.1 <= 0 {
            return true;
        }

        let width = self.width();
        let height = self.height();
        let bounding_rect = t.apply_to_rect(glyph_rect);
        let x_start = bounding_rect.origin.x.round().max(0.0) as GuestUSize;
        let y_start = bounding_rect.origin.y.round().max(0.0) as GuestUSize;
        let x_end = (bounding_rect.origin.x + bounding_rect.size.width)
            .round()
            .min(width as f32) as GuestUSize;
        let y_end = (bounding_rect.origin.y + bounding_rect.size.height)
            .round()
            .min(height as f32) as GuestUSize;
        if x_start >= x_end || y_start >= y_end {
            return true;
        }

        let inverse = t.invert();
        let Some(x_span) = axis_aligned_span(x_start, x_end, |x| {
            let u = inverse.apply_to_point(CGPoint {
                x: x as f32 + 0.5,
                y: 0.5,
            });
            let w = (u.x - glyph_rect.origin.x) / glyph_rect.size.width;
            (0.0..1.0).contains(&w)
        }) else {
            return false;
        };
        let Some((x0, x1)) = x_span else {
            return true;
        };
        let Some(y_span) = axis_aligned_span(y_start, y_end, |y| {
            let u = inverse.apply_to_point(CGPoint {
                x: 0.5,
                y: y as f32 + 0.5,
            });
            let w = (u.y - glyph_rect.origin.y) / glyph_rect.size.height;
            (0.0..1.0).contains(&w)
        }) else {
            return false;
        };
        let Some((y0, y1)) = y_span else {
            return true;
        };

        let u0 = inverse.apply_to_point(CGPoint {
            x: x0 as f32 + 0.5,
            y: y0 as f32 + 0.5,
        });
        let sx0 = (u0.x - glyph_rect.origin.x - 0.5).round() as i32;
        let sy0 = (u0.y - glyph_rect.origin.y - 0.5).round() as i32;
        let step_x = if inverse.a > 0.0 { 1 } else { -1 };
        let step_y = if inverse.d > 0.0 { 1 } else { -1 };

        let Some((x0, x1, sx0)) = clip_src_axis(x0, x1, sx0, step_x, raster_dims.0) else {
            return true;
        };
        let Some((y0, y1, mut sy)) = clip_src_axis(y0, y1, sy0, step_y, raster_dims.1) else {
            return true;
        };

        let bpr = self.bitmap_info.bytes_per_row as usize;
        let rw = raster_dims.0 as usize;
        let (fr, fg, fb, fa) = fill_color;
        let mut drawn_pixels = 0u64;

        if is_alpha_only {
            for y in y0..=y1 {
                let dst_row = ((height - 1 - y) * self.bitmap_info.bytes_per_row) as usize;
                let src_row = (sy as usize) * rw;
                let mut sx = sx0;
                for x in x0..=x1 {
                    let cov = raster_pixels[src_row + sx as usize];
                    sx += step_x;
                    if cov == 0.0 {
                        continue;
                    }
                    drawn_pixels += 1;
                    let idx = dst_row + x as usize;
                    let fg_a = fa * cov;
                    let out_a = if cov == 1.0 && fa == 1.0 {
                        1.0
                    } else {
                        let bg_a = self.pixels[idx] as f32 / 255.0;
                        fg_a + bg_a * (1.0 - fg_a)
                    };
                    self.pixels[idx] = (out_a * 255.0) as u8;
                }
                sy += step_y;
            }
            if drawn_pixels > 0 {
                n72_cg_stats::count(drawn_pixels);
            }
            return true;
        }

        let (r_off, g_off, b_off, a_off_opt) = self.pixel_offset;
        let a_off = a_off_opt.unwrap();
        let solid_rgb = [
            (gamma_encode_fast(fr) * 255.0) as u8,
            (gamma_encode_fast(fg) * 255.0) as u8,
            (gamma_encode_fast(fb) * 255.0) as u8,
        ];

        for y in y0..=y1 {
            let dst_row = ((height - 1 - y) as usize) * bpr;
            let src_row = (sy as usize) * rw;
            let mut sx = sx0;
            for x in x0..=x1 {
                let cov = raster_pixels[src_row + sx as usize];
                sx += step_x;
                if cov == 0.0 {
                    continue;
                }
                drawn_pixels += 1;
                let idx = dst_row + (x as usize) * 4;
                if cov == 1.0 && fa == 1.0 {
                    self.pixels[idx + r_off] = solid_rgb[0];
                    self.pixels[idx + g_off] = solid_rgb[1];
                    self.pixels[idx + b_off] = solid_rgb[2];
                    self.pixels[idx + a_off] = 255;
                    continue;
                }
                let pixel = (fr * cov, fg * cov, fb * cov, fa * cov);
                let (r, g, b, a) = if self.pixels[idx..idx + 4] == [0, 0, 0, 0] && fa > 0.0 {
                    pixel
                } else {
                    let bg = get_pixel_at(self.pixel_offset, self.pixels, idx);
                    if is_premul {
                        blend_premultiplied(bg, pixel, kCGBlendModeNormal)
                    } else {
                        blend_straight(bg, pixel, kCGBlendModeNormal)
                    }
                };
                self.pixels[idx + r_off] = (gamma_encode_fast(r) * 255.0) as u8;
                self.pixels[idx + g_off] = (gamma_encode_fast(g) * 255.0) as u8;
                self.pixels[idx + b_off] = (gamma_encode_fast(b) * 255.0) as u8;
                self.pixels[idx + a_off] = (a * 255.0) as u8;
            }
            sy += step_y;
        }
        if drawn_pixels > 0 {
            n72_cg_stats::count(drawn_pixels);
        }
        true
    }

    /// n72 fork: the pixels [Self::iter_transformed_pixels] would visit for an
    /// axis-aligned transform, set to zero a row span at a time (what
    /// put_pixel writes for a clear). Returns false (nothing done) otherwise.
    pub fn clear_rect_axis_aligned(&mut self, untransformed_rect: CGRect) -> bool {
        let t = self.transform;
        if t.b != 0.0 || t.c != 0.0 || t.a == 0.0 || t.d == 0.0 {
            return false;
        }
        let bounding_rect = t.apply_to_rect(untransformed_rect);
        let x_start = bounding_rect.origin.x.round().max(0.0) as GuestUSize;
        let y_start = bounding_rect.origin.y.round().max(0.0) as GuestUSize;
        let x_end = (bounding_rect.origin.x + bounding_rect.size.width)
            .round()
            .min(self.width() as f32) as GuestUSize;
        let y_end = (bounding_rect.origin.y + bounding_rect.size.height)
            .round()
            .min(self.height() as f32) as GuestUSize;
        let inverse = t.invert();
        // Same inside test as iter_transformed_pixels; with b = c = 0 it is
        // separable, so it is done once per column and once per row.
        let inside_x = |x: GuestUSize| {
            let u = inverse.apply_to_point(CGPoint {
                x: x as f32 + 0.5,
                y: 0.5,
            });
            let w = (u.x - untransformed_rect.origin.x) / untransformed_rect.size.width;
            (0.0..1.0).contains(&w)
        };
        let inside_y = |y: GuestUSize| {
            let u = inverse.apply_to_point(CGPoint {
                x: 0.5,
                y: y as f32 + 0.5,
            });
            let w = (u.y - untransformed_rect.origin.y) / untransformed_rect.size.height;
            (0.0..1.0).contains(&w)
        };
        let Some(span) = axis_aligned_span(x_start, x_end, inside_x) else {
            return false; // not a single span (cannot happen; be safe)
        };
        let Some((x0, x1)) = span else {
            return true; // nothing inside
        };
        let px = self.pixel_size;
        let alpha_only = self.bitmap_info.alpha_info == kCGImageAlphaOnly;
        let (width, height, bpr) = (
            self.bitmap_info.width,
            self.bitmap_info.height,
            self.bitmap_info.bytes_per_row,
        );
        assert!(x1 < width);
        for y in (y_start..y_end).filter(|&y| inside_y(y)) {
            // Rows are stored top to bottom (see put_pixel).
            let row = (height - 1 - y) * bpr;
            if alpha_only {
                for x in x0..=x1 {
                    self.pixels[(row + x * px) as usize] = 0;
                }
            } else {
                let a = (row + x0 * px) as usize;
                let b = (row + (x1 + 1) * px) as usize;
                self.pixels[a..b].fill(0);
            }
        }
        true
    }

    /// Takes a [CGRect] and applies the current transform to it, and iterates
    /// over the transformed, clipped, absolute integer pixel co-ordinates in
    /// raster order for the target bitmap while providing floating-point
    /// co-ordinates from (0,0) to (1,1) as a reference for sampling e.g. a
    /// texture.
    pub fn iter_transformed_pixels(
        &self,
        untransformed_rect: CGRect,
    ) -> impl Iterator<Item = ((i32, i32), (f32, f32))> {
        let bounding_rect = self.transform.apply_to_rect(untransformed_rect);

        let x_start = bounding_rect.origin.x.round().max(0.0) as GuestUSize;
        let y_start = bounding_rect.origin.y.round().max(0.0) as GuestUSize;
        let x_end = (bounding_rect.origin.x + bounding_rect.size.width)
            .round()
            .min(self.width() as f32) as GuestUSize;
        let y_end = (bounding_rect.origin.y + bounding_rect.size.height)
            .round()
            .min(self.height() as f32) as GuestUSize;

        let inverse_transform = self.transform.invert();

        // TODO: Doing a matrix multiply per-pixel is not optimally efficient.
        // A scanline rasterizer would be better, though we should probably use
        // an existing library for this.
        (y_start..y_end).flat_map(move |y| {
            (x_start..x_end).flat_map(move |x| {
                let untransformed = inverse_transform.apply_to_point(CGPoint {
                    x: x as f32 + 0.5,
                    y: y as f32 + 0.5,
                });
                let x_within =
                    (untransformed.x - untransformed_rect.origin.x) / untransformed_rect.size.width;
                let y_within = (untransformed.y - untransformed_rect.origin.y)
                    / untransformed_rect.size.height;
                if !(0.0..1.0).contains(&x_within) || !(0.0..1.0).contains(&y_within) {
                    None
                } else {
                    Some(((x as i32, y as i32), (x_within, y_within)))
                }
            })
        })
    }
}

fn axis_aligned_span(
    start: GuestUSize,
    end: GuestUSize,
    inside: impl Fn(GuestUSize) -> bool,
) -> Option<Option<(GuestUSize, GuestUSize)>> {
    let mut first: Option<GuestUSize> = None;
    let mut last: GuestUSize = 0;
    let mut count: GuestUSize = 0;
    for v in start..end {
        if inside(v) {
            if first.is_none() {
                first = Some(v);
            }
            last = v;
            count += 1;
        }
    }
    match first {
        None => Some(None),
        Some(a) if last - a + 1 == count => Some(Some((a, last))),
        Some(_) => None,
    }
}

fn clip_src_axis(
    a0: GuestUSize,
    a1: GuestUSize,
    mut s0: i32,
    step: i32,
    limit: i32,
) -> Option<(GuestUSize, GuestUSize, i32)> {
    let mut lo = a0;
    let mut hi = a1;
    let len = (hi - lo) as i32;
    if step > 0 {
        if s0 < 0 {
            let skip = -s0;
            if skip > len {
                return None;
            }
            lo += skip as GuestUSize;
            s0 = 0;
        }
        let s_end = s0 + (hi - lo) as i32;
        if s_end >= limit {
            let over = s_end - (limit - 1);
            if over > (hi - lo) as i32 {
                return None;
            }
            hi -= over as GuestUSize;
        }
    } else {
        if s0 >= limit {
            let skip = s0 - (limit - 1);
            if skip > len {
                return None;
            }
            lo += skip as GuestUSize;
            s0 = limit - 1;
        }
        let s_end = s0 - (hi - lo) as i32;
        if s_end < 0 {
            let over = -s_end;
            if over > (hi - lo) as i32 {
                return None;
            }
            hi -= over as GuestUSize;
        }
    }
    if lo > hi {
        None
    } else {
        Some((lo, hi, s0))
    }
}

#[cfg(test)]
#[test]
fn test_iter_transformed_pixels() {
    use super::CGSize;

    fn make_context(
        width: GuestUSize,
        height: GuestUSize,
        transform: CGAffineTransform,
    ) -> CGBitmapContextDrawer<'static> {
        CGBitmapContextDrawer {
            bitmap_info: CGBitmapContextData {
                data: crate::mem::Ptr::null(),
                data_is_owned: false,
                width,
                height,
                bits_per_component: 8,
                bytes_per_row: 3 * width,
                color_space: "kCGColorSpaceGenericRGB",
                alpha_info: 0,
            },
            pixel_offset: (0, 1, 2, None),
            pixel_size: 3,
            rgb_fill_color: (0.0, 0.0, 0.0, 0.0),
            blend_mode: kCGBlendModeNormal,
            transform,
            pixels: &mut [],
        }
    }

    let square_2x2_at_0_0 = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 2.0,
            height: 2.0,
        },
    };
    let square_2x2_at_2_2 = CGRect {
        origin: CGPoint { x: 2.0, y: 2.0 },
        size: CGSize {
            width: 2.0,
            height: 2.0,
        },
    };
    let square_4x4_at_0_0 = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 4.0,
            height: 4.0,
        },
    };

    let upright_square_2x2_at_0_0 = [
        ((0, 0), (0.25, 0.25)),
        ((1, 0), (0.75, 0.25)),
        ((0, 1), (0.25, 0.75)),
        ((1, 1), (0.75, 0.75)),
    ];
    let inverted_square_2x2_at_0_0 = [
        ((0, 0), (0.75, 0.75)),
        ((1, 0), (0.25, 0.75)),
        ((0, 1), (0.75, 0.25)),
        ((1, 1), (0.25, 0.25)),
    ];
    let corner_pixel_of_upright_square_2x2_at_1_1 = [((1, 1), (0.25, 0.25))];

    // Constructed by hand so the results are precise
    let rotation_by_180deg = CGAffineTransform {
        a: -1.0,
        c: 0.0,
        b: 0.0,
        d: -1.0,
        tx: 0.0,
        ty: 0.0,
    };

    assert!(make_context(2, 2, CGAffineTransformIdentity)
        .iter_transformed_pixels(square_2x2_at_0_0)
        .eq(upright_square_2x2_at_0_0.into_iter()));
    assert!(
        make_context(2, 2, CGAffineTransform::make_translation(-2.0, -2.0))
            .iter_transformed_pixels(square_2x2_at_2_2)
            .eq(upright_square_2x2_at_0_0.into_iter())
    );
    assert!(
        make_context(2, 2, CGAffineTransform::make_translation(-1.0, -1.0))
            .iter_transformed_pixels(square_2x2_at_2_2)
            .eq(corner_pixel_of_upright_square_2x2_at_1_1.into_iter())
    );
    assert!(make_context(2, 2, CGAffineTransform::make_scale(0.5, 0.5))
        .iter_transformed_pixels(square_4x4_at_0_0)
        .eq(upright_square_2x2_at_0_0.into_iter()));
    assert!(make_context(2, 2, rotation_by_180deg.translate(-2.0, -2.0))
        .iter_transformed_pixels(square_2x2_at_0_0)
        .eq(inverted_square_2x2_at_0_0.into_iter()));
}

#[cfg(test)]
#[test]
fn test_draw_glyph_axis_aligned() {
    use super::CGSize;

    fn make_drawer<'a>(
        width: GuestUSize,
        height: GuestUSize,
        alpha_info: CGImageAlphaInfo,
        transform: CGAffineTransform,
        pixels: &'a mut [u8],
    ) -> CGBitmapContextDrawer<'a> {
        let px = if alpha_info == kCGImageAlphaOnly { 1 } else { 4 };
        let bitmap_info = CGBitmapContextData {
            data: crate::mem::Ptr::null(),
            data_is_owned: false,
            width,
            height,
            bits_per_component: 8,
            bytes_per_row: px * width,
            color_space: kCGColorSpaceGenericRGB,
            alpha_info,
        };
        CGBitmapContextDrawer {
            pixel_offset: pixel_offsets(&bitmap_info),
            pixel_size: px,
            bitmap_info,
            rgb_fill_color: (0.0, 0.0, 0.0, 0.0),
            blend_mode: kCGBlendModeNormal,
            transform,
            pixels,
        }
    }

    let w = 16u32;
    let h = 16u32;
    let raster_dims = (6i32, 5i32);
    let raster_pixels: Vec<f32> = (0..(raster_dims.0 * raster_dims.1))
        .map(|i| match i % 5 {
            0 => 0.0,
            1 => 1.0,
            2 => 0.25,
            3 => 0.5,
            _ => 0.875,
        })
        .collect();

    let alpha_infos = [
        kCGImageAlphaPremultipliedLast,
        kCGImageAlphaPremultipliedFirst,
        kCGImageAlphaLast,
        kCGImageAlphaFirst,
        kCGImageAlphaOnly,
    ];
    let transforms = [
        CGAffineTransformIdentity,
        CGAffineTransform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            tx: 0.0,
            ty: h as f32,
        },
        CGAffineTransform::make_translation(-2.0, -1.0),
        CGAffineTransform {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            tx: w as f32 + 1.25,
            ty: h as f32,
        },
    ];
    let rects = [
        CGRect {
            origin: CGPoint { x: 3.0, y: 5.0 },
            size: CGSize {
                width: 6.0,
                height: 5.0,
            },
        },
        CGRect {
            // partially clipped right/bottom
            origin: CGPoint { x: 1.0, y: 2.0 },
            size: CGSize {
                width: 4.5,
                height: 3.5,
            },
        },
    ];
    let colors = [(1.0, 1.0, 1.0, 1.0), (0.9, 0.45, 0.2, 0.75)];
    let bgs: [[u8; 4]; 3] = [[0, 0, 0, 0], [40, 80, 120, 128], [200, 150, 50, 255]];

    for &alpha_info in &alpha_infos {
        let px_bytes = if alpha_info == kCGImageAlphaOnly { 1 } else { 4 };
        for &transform in &transforms {
            for &rect in &rects {
                for &color in &colors {
                    for &bg in &bgs {
                        let init: Vec<u8> = (0..(w * h) as usize)
                            .flat_map(|_| bg[..px_bytes].iter().copied())
                            .collect();
                        let mut expected = init.clone();
                        let mut actual = init;

                        {
                            let mut d_exp = make_drawer(w, h, alpha_info, transform, &mut expected);
                            let skip_uncovered = d_exp.blends_normally();
                            for ((x, y), (tx, ty)) in d_exp.iter_transformed_pixels(rect) {
                                let sx = (tx * rect.size.width - 0.5).round() as i32;
                                let sy = (ty * rect.size.height - 0.5).round() as i32;
                                let cov = if (0..raster_dims.0).contains(&sx)
                                    && (0..raster_dims.1).contains(&sy)
                                {
                                    raster_pixels[sy as usize * raster_dims.0 as usize + sx as usize]
                                } else {
                                    0.0
                                };
                                if cov == 0.0 && skip_uncovered {
                                    continue;
                                }
                                let p = (color.0 * cov, color.1 * cov, color.2 * cov, color.3 * cov);
                                d_exp.put_pixel((x, y), p, true);
                            }
                        }

                        {
                            let mut d_act = make_drawer(w, h, alpha_info, transform, &mut actual);
                            assert!(d_act.draw_glyph_axis_aligned(
                                rect,
                                raster_dims,
                                &raster_pixels,
                                color,
                            ));
                        }

                        assert_eq!(
                            actual, expected,
                            "mismatch for alpha_info={alpha_info} transform={transform:?} rect={rect:?} color={color:?} bg={bg:?}"
                        );
                    }
                }
            }
        }
    }
}

/// Implementation of `CGContextFillRect` (`clear` == [false]) and
/// `CGContextClearRect` (`clear` == [true]) for `CGBitmapContext`.
pub(super) fn fill_rect(env: &mut Environment, context: CGContextRef, rect: CGRect, clear: bool) {
    n72_cg_stats::op(if clear { "clear" } else { "fill" });
    let mut drawer = CGBitmapContextDrawer::new(&env.objc, &mut env.mem, context);
    // n72 fork: clearing an axis-aligned rectangle zeroes whole row spans
    // instead of blending pixel by pixel (Spore clears ~13k pixels a frame).
    if clear && drawer.clear_rect_axis_aligned(rect) {
        return;
    }
    let color = if clear {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        drawer.rgb_fill_color()
    };
    // TODO: correct anti-aliasing
    for ((x, y), _) in drawer.iter_transformed_pixels(rect) {
        drawer.put_pixel((x, y), color, /* blend: */ !clear)
    }
}

/// Implementation of `CGContextDrawImage` for `CGBitmapContext`.
pub(super) fn draw_image(
    env: &mut Environment,
    context: CGContextRef,
    rect: CGRect,
    image: CGImageRef,
) {
    n72_cg_stats::op("image");
    let image = cg_image::borrow_image(&env.objc, image);

    let mut drawer = CGBitmapContextDrawer::new(&env.objc, &mut env.mem, context);

    //let _ = std::fs::write(
    //  format!(
    //      "image-{:?}-{:?}.data",
    //      (image as *const _ as *const ()),
    //      image.dimensions()
    //  ),
    //  image.pixels()
    //);

    //let _ = std::fs::write(
    //  format!(
    //      "bitmap-{:?}-{:?}-before.data",
    //      (image as *const _ as *const ()),
    //      (drawer.width(), drawer.height())
    //  ),
    //  &drawer.pixels
    //);

    let (image_width, image_height) = image.dimensions();

    // TODO: non-nearest-neighbour filtering? (what does CG actually do?)

    for ((x, y), (texel_x, texel_y)) in drawer.iter_transformed_pixels(rect) {
        let texel_x = (image_width as f32 * texel_x) as i32;
        // Image is in top-to-bottom order, but the bitmap is bottom-to-top
        let texel_y = (image_height as f32 * (1.0 - texel_y)) as i32;
        // FIXME: might need alpha format conversion here
        if let Some(color) = image.get_pixel((texel_x, texel_y)) {
            drawer.put_pixel((x, y), color, /* blend: */ true)
        }
    }

    //let _ = std::fs::write(
    //  format!(
    //      "bitmap-{:?}-{:?}-after.data",
    //      (image as *const _ as *const ()),
    //      (drawer.width(), drawer.height())
    //  ),
    //  &drawer.pixels
    //);
}

#[allow(rustdoc::broken_intra_doc_links)] // https://github.com/rust-lang/rust/issues/83049
/// Shortcut for [crate::frameworks::core_animation::composition]. This is a
/// workaround for not having a `&mut Environment` that should eventually be
/// removed somehow (TODO).
pub fn get_data(objc: &ObjC, context: CGContextRef) -> (GuestUSize, GuestUSize, MutVoidPtr) {
    let host_obj = objc.borrow::<CGContextHostObject>(context);
    let CGContextSubclass::CGBitmapContext(bitmap_data) = host_obj.subclass;
    (bitmap_data.width, bitmap_data.height, bitmap_data.data)
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CGBitmapContextCreate(_, _, _, _, _, _, _)),
    export_c_func!(CGBitmapContextCreateImage(_)),
    export_c_func!(CGBitmapContextGetData(_)),
    export_c_func!(CGBitmapContextGetWidth(_)),
    export_c_func!(CGBitmapContextGetHeight(_)),
    export_c_func!(CGBitmapContextGetBytesPerRow(_)),
];

/// n72 fork, profiling aid (`n72-prof` feature): with N72_CG_STATS set,
/// log once a second how many pixels the software Core Graphics drew per kind
/// of operation.
#[cfg(feature = "n72-prof")]
pub(crate) mod n72_cg_stats {
    use std::cell::RefCell;
    use std::time::Instant;

    struct Stats {
        on: bool,
        current: &'static str,
        counts: Vec<(&'static str, u64, u64)>, // (op, calls, pixels)
        since: Instant,
    }

    /// N72_CG_STATS, read once: when it is off (always, outside profiling)
    /// the per-pixel count is one load, not a thread-local lookup.
    fn on() -> bool {
        static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *ON.get_or_init(|| std::env::var_os("N72_CG_STATS").is_some())
    }

    thread_local! {
        static STATS: RefCell<Stats> = RefCell::new(Stats {
            on: std::env::var_os("N72_CG_STATS").is_some(),
            current: "other",
            counts: Vec::new(),
            since: Instant::now(),
        });
    }

    pub fn op(name: &'static str) {
        if !on() {
            return;
        }
        STATS.with(|s| {
            let mut s = s.borrow_mut();
            if !s.on {
                return;
            }
            s.current = name;
            match s.counts.iter_mut().find(|c| c.0 == name) {
                Some(c) => c.1 += 1,
                None => s.counts.push((name, 1, 0)),
            }
        })
    }

    #[inline]
    pub fn count(pixels: u64) {
        if !on() {
            return;
        }
        STATS.with(|s| {
            let mut s = s.borrow_mut();
            if !s.on {
                return;
            }
            let name = s.current;
            match s.counts.iter_mut().find(|c| c.0 == name) {
                Some(c) => c.2 += pixels,
                None => s.counts.push((name, 0, pixels)),
            }
            if s.since.elapsed().as_secs() >= 1 {
                let line: Vec<String> = s
                    .counts
                    .iter()
                    .map(|(n, calls, px)| format!("{n} {calls} calls {px} px"))
                    .collect();
                log!("n72 CG stats/s: {}", line.join(", "));
                s.counts.clear();
                s.since = Instant::now();
            }
        })
    }
}

#[cfg(not(feature = "n72-prof"))]
pub(crate) mod n72_cg_stats {
    #[inline(always)]
    pub fn op(_: &'static str) {}
    #[inline(always)]
    pub fn count(_: u64) {}
}
