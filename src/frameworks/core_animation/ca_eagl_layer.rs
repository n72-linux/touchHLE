/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CAEAGLLayer`.

use super::ca_layer::CALayerHostObject;
use crate::frameworks::core_graphics::cg_affine_transform::CGAffineTransform;
use crate::frameworks::core_graphics::{CGPoint, CGRect};
use crate::objc::{id, msg, msg_class, nil, objc_classes, Class, ClassExports};
use crate::Environment;

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation CAEAGLLayer: CALayer

// EAGLDrawable implementation (the only one)

- (id)drawableProperties {
    // FIXME: do we need to return an empty dictionary rather than nil?
    env.objc.borrow::<CALayerHostObject>(this).drawable_properties
}

- (())setDrawableProperties:(id)props { // NSDictionary<NSString*, id>*
    let props: id = msg![env; props copy];
    env.objc.borrow_mut::<CALayerHostObject>(this).drawable_properties = props;
}

@end

};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FullscreenEaglRotation {
    Identity,
    /// +90° clockwise in UIKit Y-down space (UIInterfaceOrientationLandscapeRight)
    Rotate90Cw,
    /// 180° (UIInterfaceOrientationPortraitUpsideDown)
    Rotate180,
    /// -90° counter-clockwise in UIKit Y-down space (UIInterfaceOrientationLandscapeLeft)
    Rotate90Ccw,
}

fn classify_quarter_turn(t: CGAffineTransform) -> Option<FullscreenEaglRotation> {
    const EPS: f32 = 1e-4;
    if t.tx.abs() >= EPS || t.ty.abs() >= EPS {
        return None;
    }
    if (t.a - 1.0).abs() < EPS && t.b.abs() < EPS && t.c.abs() < EPS && (t.d - 1.0).abs() < EPS {
        Some(FullscreenEaglRotation::Identity)
    } else if t.a.abs() < EPS && (t.b - 1.0).abs() < EPS && (t.c + 1.0).abs() < EPS && t.d.abs() < EPS {
        Some(FullscreenEaglRotation::Rotate90Cw)
    } else if (t.a + 1.0).abs() < EPS && t.b.abs() < EPS && t.c.abs() < EPS && (t.d + 1.0).abs() < EPS {
        Some(FullscreenEaglRotation::Rotate180)
    } else if t.a.abs() < EPS && (t.b + 1.0).abs() < EPS && (t.c - 1.0).abs() < EPS && t.d.abs() < EPS {
        Some(FullscreenEaglRotation::Rotate90Ccw)
    } else {
        None
    }
}

/// If there is an opaque `CAEAGLLayer` that covers the entire screen
/// (including quarter-turn rotated fullscreen layers),
/// returns `(layer, rotation)`. Otherwise returns `None`.
pub fn find_fullscreen_eagl_layer_with_rotation(
    env: &mut Environment,
) -> Option<(id, FullscreenEaglRotation)> {
    if env.options.force_composition {
        return None;
    }

    let windows = env.framework_state.uikit.ui_view.ui_window.windows.clone();
    // Assumes the windows in the list are ordered back-to-front.
    // TODO: this may not be correct once we support windowLevel.
    let top_window = windows
        .into_iter()
        .rev()
        .find(|&window| !msg![env; window isHidden])?;

    let screen_bounds: CGRect = {
        let screen: id = msg_class![env; UIScreen mainScreen];
        msg![env; screen bounds]
    };

    let mut layer: id = msg![env; top_window layer];

    // Descend through the hierarchy, looking only at the last layer in each
    // list of children, since that should be the one on top.
    // TODO: this is not correct once we support zPosition.
    let rotation = loop {
        assert!(layer != nil);

        let layer_host_obj: &CALayerHostObject = env.objc.borrow(layer);

        // n72 fork: hidden or fully transparent sublayers draw nothing, so
        // they must not stop the descent (Spore's GL layer carries a hidden
        // news ticker as its last sublayer, which forced composition every
        // frame).
        let visible_last = layer_host_obj.sublayers.iter().rev().copied().find(|&sub| {
            let h = env.objc.borrow::<CALayerHostObject>(sub);
            !h.hidden && h.opacity > 0.0
        });

        if let Some(next) = visible_last {
            // Intermediate ancestor layer: keep the existing strict identity check.
            if layer_host_obj.bounds.size != screen_bounds.size
                || layer_host_obj.bounds.origin != (CGPoint { x: 0.0, y: 0.0 })
                || layer_host_obj.anchor_point != (CGPoint { x: 0.5, y: 0.5 })
                || layer_host_obj.position
                    != (CGPoint {
                        x: screen_bounds.size.width / 2.0,
                        y: screen_bounds.size.height / 2.0,
                    })
                || layer_host_obj.hidden
                || layer_host_obj.opacity != 1.0
                || !layer_host_obj.affine_transform.is_identity()
            {
                return None;
            }
            layer = next;
        } else {
            let rot = classify_quarter_turn(layer_host_obj.affine_transform)?;
            let (expected_w, expected_h) = match rot {
                FullscreenEaglRotation::Identity | FullscreenEaglRotation::Rotate180 => {
                    (screen_bounds.size.width, screen_bounds.size.height)
                }
                FullscreenEaglRotation::Rotate90Cw | FullscreenEaglRotation::Rotate90Ccw => {
                    (screen_bounds.size.height, screen_bounds.size.width)
                }
            };
            if (layer_host_obj.bounds.size.width - expected_w).abs() >= 0.5
                || (layer_host_obj.bounds.size.height - expected_h).abs() >= 0.5
                || layer_host_obj.bounds.origin != (CGPoint { x: 0.0, y: 0.0 })
                || layer_host_obj.anchor_point != (CGPoint { x: 0.5, y: 0.5 })
                || (layer_host_obj.position.x - screen_bounds.size.width / 2.0).abs() >= 0.5
                || (layer_host_obj.position.y - screen_bounds.size.height / 2.0).abs() >= 0.5
                || layer_host_obj.hidden
                || layer_host_obj.opacity != 1.0
                || !layer_host_obj.animations.is_empty()
                || !layer_host_obj.anonymous_animations.is_empty()
            {
                return None;
            }
            break rot;
        }
    };

    if !env.objc.borrow::<CALayerHostObject>(layer).opaque {
        return None;
    }

    let ca_eagl_layer_class: Class = msg_class![env; CAEAGLLayer class];
    if !msg![env; layer isKindOfClass:ca_eagl_layer_class] {
        return None;
    }

    Some((layer, rotation))
}

/// If there is an opaque `CAEAGLLayer` that covers the entire screen, this
/// returns a pointer to it. Otherwise, it returns [nil].
///
/// To avoid a state management nightmare, we want to have an internal OpenGL ES
/// context for compositing, separate from any OpenGL ES contexts the app uses
/// for its rendering. When we have a `CAEAGLLayer` though, we need to transfer
/// a rendered frame from the app's context to the compositor's context, and
/// unfortunately the most practical way to do this is `glReadPixels()`, which
/// is highly inefficient. To make things efficient, then, we have a shortcut:
/// if the result of composition would be identical to the rendered frame, i.e.
/// there's a single full-screen layer, we skip transferring between contexts
/// and present it directly from the app's context. This function is used to
/// determine when that will happen.
pub fn find_fullscreen_eagl_layer(env: &mut Environment) -> id {
    find_fullscreen_eagl_layer_with_rotation(env).map_or(nil, |(l, _)| l)
}

/// n72 fork: like [find_fullscreen_eagl_layer], but allows visible overlay
/// layers (sublayers of the `CAEAGLLayer` or later siblings in the top window)
/// on top of an opaque full-screen identity-transform `CAEAGLLayer`.
///
/// Returns `(eagl_layer, overlay_roots)` in back-to-front order so the
/// `CAEAGLLayer` can stay window-backed and only its overlays are blended on
/// top of the panel back buffer on present — avoiding the GPU copy + full-
/// screen clear + 320x480 blit that Core Animation composition otherwise does
/// (Spore's menu carries an 8x16 ticker tab and an off-screen ticker).
pub fn find_fullscreen_eagl_base(env: &mut Environment) -> Option<(id, Vec<id>)> {
    if env.options.force_composition
        || !crate::window::Window::fixed_portrait_panel()
        || env.options.scale_hack.get() != 1
    {
        return None;
    }

    let screen_bounds: CGRect = {
        let screen: id = msg_class![env; UIScreen mainScreen];
        msg![env; screen bounds]
    };

    let window_ok = env.window.as_ref().is_some_and(|w| {
        w.virtual_cursor_visible_at().is_none()
            && w.viewport()
                == (
                    0,
                    0,
                    screen_bounds.size.width as u32,
                    screen_bounds.size.height as u32,
                )
    });
    if !window_ok {
        return None;
    }

    let windows = env.framework_state.uikit.ui_view.ui_window.windows.clone();
    let top_window = windows
        .into_iter()
        .rev()
        .find(|&window| !msg![env; window isHidden])?;

    let root_layer: id = msg![env; top_window layer];
    let ca_eagl_layer_class: Class = msg_class![env; CAEAGLLayer class];

    fn is_fullscreen_identity(h: &CALayerHostObject, screen_bounds: CGRect) -> bool {
        h.bounds.size == screen_bounds.size
            && h.bounds.origin == (CGPoint { x: 0.0, y: 0.0 })
            && h.anchor_point == (CGPoint { x: 0.5, y: 0.5 })
            && h.position
                == (CGPoint {
                    x: screen_bounds.size.width / 2.0,
                    y: screen_bounds.size.height / 2.0,
                })
            && !h.hidden
            && h.opacity == 1.0
            && h.affine_transform.is_identity()
            && h.animations.is_empty()
            && h.anonymous_animations.is_empty()
    }

    fn find_in(
        env: &mut Environment,
        layer: id,
        screen_bounds: CGRect,
        ca_eagl_layer_class: Class,
    ) -> Option<(id, Vec<id>)> {
        let (ok, opaque_plain, sublayers) = {
            let h = env.objc.borrow::<CALayerHostObject>(layer);
            (
                is_fullscreen_identity(h, screen_bounds),
                h.opaque && h.corner_radius == 0.0,
                h.sublayers.clone(),
            )
        };
        if !ok {
            return None;
        }
        if opaque_plain && msg![env; layer isKindOfClass:ca_eagl_layer_class] {
            return Some((layer, sublayers));
        }
        for idx in (0..sublayers.len()).rev() {
            let child = sublayers[idx];
            {
                let h = env.objc.borrow::<CALayerHostObject>(child);
                if h.hidden || h.opacity <= 0.0 {
                    continue;
                }
            }
            if let Some((base, mut overlays)) =
                find_in(env, child, screen_bounds, ca_eagl_layer_class)
            {
                overlays.extend_from_slice(&sublayers[idx + 1..]);
                return Some((base, overlays));
            }
        }
        None
    }

    find_in(env, root_layer, screen_bounds, ca_eagl_layer_class)
}

/// n72 fork: whether this `CAEAGLLayer` currently renders straight into the
/// window back buffer.
pub fn layer_is_window_backed(env: &Environment, layer: id) -> bool {
    env.objc.borrow::<CALayerHostObject>(layer).n72_window_backed
}

/// n72 fork: record whether this `CAEAGLLayer` renders straight into the
/// window back buffer.
pub fn set_layer_window_backed(env: &mut Environment, layer: id, on: bool) {
    env.objc
        .borrow_mut::<CALayerHostObject>(layer)
        .n72_window_backed = on;
}

/// For use by `EAGLContext` when presenting to a `CAEAGLLayer`:
/// [std::mem::take]s the buffer used to hold the pixels. It should be passed
/// back to [present_pixels] once it has been filled.
pub fn get_pixels_vec_for_presenting(env: &mut Environment, layer: id) -> Vec<u8> {
    env.objc
        .borrow_mut::<CALayerHostObject>(layer)
        .presented_pixels
        .take()
        .map(|(vec, _width, _height)| vec)
        .unwrap_or_default()
}

/// For use by `EAGLContext` when presenting to a `CAEAGLLayer`: provide the new
/// frame rendered by the app, so it can be used when compositing. The buffer
/// should have been obtained with [get_pixels_vec_for_presenting] before
/// filling. The data must be in RGBA8 format.
pub fn present_pixels(env: &mut Environment, layer: id, pixels: Vec<u8>, width: u32, height: u32) {
    let host_obj = env.objc.borrow_mut::<CALayerHostObject>(layer);
    host_obj.presented_pixels = Some((pixels, width, height));
    host_obj.gles_texture_is_up_to_date = false;
}

/// n72 fork: the texture (shared with the compositor) that last held this
/// layer's frame, to be reused for the next one.
pub fn presented_texture_for(
    env: &mut Environment,
    layer: id,
) -> Option<crate::gles::gles11_raw::types::GLuint> {
    env.objc
        .borrow::<CALayerHostObject>(layer)
        .presented_texture
        .map(|(texture, _, _)| texture)
}

/// n72 fork: whether the layer is opaque (its alpha channel is ignored).
pub fn layer_is_opaque(env: &mut Environment, layer: id) -> bool {
    env.objc.borrow::<CALayerHostObject>(layer).opaque
}

/// n72 fork: like [present_pixels], but the frame is already in `texture`, a
/// texture object shared with the compositor's GL context (RGBA, rows bottom
/// to top as glReadPixels would give them).
pub fn present_texture(
    env: &mut Environment,
    layer: id,
    texture: crate::gles::gles11_raw::types::GLuint,
    width: u32,
    height: u32,
) {
    let host_obj = env.objc.borrow_mut::<CALayerHostObject>(layer);
    host_obj.presented_texture = Some((texture, width, height));
    host_obj.presented_pixels = None;
    host_obj.gles_texture_is_up_to_date = false;
}

#[cfg(feature = "n72-prof")]
/// n72 fork, debugging aid: log a window's layer tree (class, geometry,
/// visibility) once, to see whether an app's GL layer is the bottom-most
/// full-screen content (N72_LAYER_DUMP).
pub fn n72_dump_layer_tree(env: &mut Environment, layer: id, depth: usize) {
    let class: Class = msg![env; layer class];
    let name = env.objc.get_class_name(class).to_string();
    let (bounds, position, hidden, opaque, opacity, identity, sublayers, cg, pixels, tex) = {
        let h = env.objc.borrow::<CALayerHostObject>(layer);
        (
            h.bounds,
            h.position,
            h.hidden,
            h.opaque,
            h.opacity,
            h.affine_transform.is_identity(),
            h.sublayers.clone(),
            h.cg_context.is_some(),
            h.presented_pixels.is_some(),
            h.presented_texture.is_some(),
        )
    };
    log!(
        "n72 layers: {:indent$}{} {:?} bounds {:?} pos {:?} hidden {} opaque {} opacity {} identity {} cg {} eagl-pixels {} eagl-tex {}",
        "",
        name,
        layer,
        bounds,
        position,
        hidden,
        opaque,
        opacity,
        identity,
        cg,
        pixels,
        tex,
        indent = depth * 2
    );
    for sub in sublayers {
        n72_dump_layer_tree(env, sub, depth + 1);
    }
}
