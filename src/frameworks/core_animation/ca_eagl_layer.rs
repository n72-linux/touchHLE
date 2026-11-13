/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CAEAGLLayer`.

use super::ca_layer::CALayerHostObject;
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
    if env.options.force_composition {
        return nil;
    }

    let windows = env.framework_state.uikit.ui_view.ui_window.windows.clone();
    // Assumes the windows in the list are ordered back-to-front.
    // TODO: this may not be correct once we support windowLevel.
    let Some(top_window) = windows
        .into_iter()
        .rev()
        .find(|&window| !msg![env; window isHidden])
    else {
        return nil;
    };

    let screen_bounds: CGRect = {
        let screen: id = msg_class![env; UIScreen mainScreen];
        msg![env; screen bounds]
    };

    let mut layer: id = msg![env; top_window layer];

    // Descend through the hierarchy, looking only at the last layer in each
    // list of children, since that should be the one on top.
    // TODO: this is not correct once we support zPosition.
    loop {
        assert!(layer != nil);

        let layer_host_obj: &CALayerHostObject = env.objc.borrow(layer);

        // This is stricter than it should be. In theory we should accumulate
        // the transforms and handle different anchor points etc, but real apps
        // probably only use this common case.
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
            // TODO: support affine transforms that result in a full-screen
            //       layer (typical example is 90° rotation).
            || !layer_host_obj.affine_transform.is_identity()
        {
            return nil;
        }

        // n72 fork: hidden or fully transparent sublayers draw nothing, so
        // they must not stop the descent (Spore's GL layer carries a hidden
        // news ticker as its last sublayer, which forced composition every
        // frame).
        let visible_last = layer_host_obj.sublayers.iter().rev().copied().find(|&sub| {
            let h = env.objc.borrow::<CALayerHostObject>(sub);
            !h.hidden && h.opacity > 0.0
        });
        if let Some(next) = visible_last {
            layer = next;
        } else {
            break;
        }
    }

    if !env.objc.borrow::<CALayerHostObject>(layer).opaque {
        return nil;
    }

    let ca_eagl_layer_class: Class = msg_class![env; CAEAGLLayer class];
    if !msg![env; layer isKindOfClass:ca_eagl_layer_class] {
        return nil;
    }

    layer
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
