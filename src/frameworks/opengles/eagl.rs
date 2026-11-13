/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! EAGL.

use crate::dyld::{ConstantExports, HostConstant};
use crate::frameworks::core_animation::ca_eagl_layer::{
    find_fullscreen_eagl_layer, get_pixels_vec_for_presenting, present_pixels,
};
use crate::frameworks::core_graphics::{CGRect, CGSize};
use crate::frameworks::foundation::ns_string::get_static_str;
use crate::frameworks::foundation::NSUInteger;
use crate::gles::gles11_raw as gles11; // constants only
use crate::gles::gles11_raw::types::*;
use crate::gles::present::{present_frame, FpsCounter};
use crate::gles::{create_gles1_ctx, gles1_on_gl2, GLESContext, GLES};
use crate::mem::MutPtr;
use crate::objc::{id, msg, nil, objc_classes, release, retain, ClassExports, HostObject};
use crate::options::Options;
use crate::Environment;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

// These are used by the EAGLDrawable protocol implemented by CAEAGLayer.
// Since these have the ABI of constant symbols rather than literal constants,
// the values shouldn't matter, and haven't been checked against real iPhone OS.
pub const kEAGLDrawablePropertyColorFormat: &str = "ColorFormat";
pub const kEAGLDrawablePropertyRetainedBacking: &str = "RetainedBacking";
pub const kEAGLColorFormatRGBA8: &str = "RGBA8";
pub const kEAGLColorFormatRGB565: &str = "RGB565";

pub const CONSTANTS: ConstantExports = &[
    (
        "_kEAGLDrawablePropertyColorFormat",
        HostConstant::NSString(kEAGLDrawablePropertyColorFormat),
    ),
    (
        "_kEAGLDrawablePropertyRetainedBacking",
        HostConstant::NSString(kEAGLDrawablePropertyRetainedBacking),
    ),
    (
        "_kEAGLColorFormatRGBA8",
        HostConstant::NSString(kEAGLColorFormatRGBA8),
    ),
    (
        "_kEAGLColorFormatRGB565",
        HostConstant::NSString(kEAGLColorFormatRGB565),
    ),
];

type EAGLRenderingAPI = u32;
const kEAGLRenderingAPIOpenGLES1: EAGLRenderingAPI = 1;
#[allow(dead_code)]
const kEAGLRenderingAPIOpenGLES2: EAGLRenderingAPI = 2;
#[allow(dead_code)]
const kEAGLRenderingAPIOpenGLES3: EAGLRenderingAPI = 3;

pub(super) struct EAGLContextHostObject {
    pub(super) gles_ctx: Option<Box<dyn GLESContext>>,
    /// Mapping of OpenGL ES renderbuffer names to `EAGLDrawable` instances
    /// (always `CAEAGLLayer*`). Retains the instance so it won't dangle.
    renderbuffer_drawable_bindings: Rc<RefCell<HashMap<GLuint, id>>>,
    fps_counter: Option<FpsCounter>,
    next_frame_due: Option<Instant>,
    pub mapped_buffers: HashMap<GLuint, (MutPtr<GLvoid>, *mut GLvoid)>,
}
impl HostObject for EAGLContextHostObject {}

pub const CLASSES: ClassExports = objc_classes! {

(env, this, _cmd);

@implementation EAGLContext: NSObject

+ (id)alloc {
    let host_object = Box::new(EAGLContextHostObject {
        gles_ctx: None,
        renderbuffer_drawable_bindings: Rc::new(RefCell::new(HashMap::new())),
        fps_counter: None,
        next_frame_due: None,
        mapped_buffers: HashMap::new(),
    });
    env.objc.alloc_object(this, host_object, &mut env.mem)
}

+ (id)currentContext {
    env.framework_state.opengles.current_ctx_for_thread(env.current_thread).unwrap_or(nil)
}
+ (bool)setCurrentContext:(id)context { // EAGLContext*
    retain(env, context);

    let current_ctx = env.framework_state.opengles.current_ctx_for_thread(env.current_thread);

    if let Some(old_ctx) = std::mem::take(current_ctx) {
        release(env, old_ctx);
    }

    // reborrow
    let current_ctx = env.framework_state.opengles.current_ctx_for_thread(env.current_thread);

    if context != nil {
        *current_ctx = Some(context);
    }

    true
}

- (id)initWithAPI:(EAGLRenderingAPI)api sharegroup:(id)group {
    if api != kEAGLRenderingAPIOpenGLES1 {
        log!(
            "TODO: App requested EAGL initWithAPI:{} sharegroup:{:?}, returning nil as we only support API 1 for now",
            api,
            group
        );
        return nil;
    }

    if group == nil {
        return msg![env; this initWithAPI:api];
    }

    let window = env.window.as_mut().expect("OpenGL ES is not supported in headless mode");
    let prev_context = env.objc.borrow_mut::<EAGLContextHostObject>(group).gles_ctx.as_mut().unwrap();

    // This is sort of a hack - we set the "current" context, then immediately
    // drop it. Since we know all the code between here and creating the new
    // context, we know that there won't be any context switches, so it's fine
    // to do this.
    {
        let _prev_ctx = prev_context.make_current(window);
    }
    env.window.as_mut().unwrap().set_share_with_current_context(true);

    let mut gles1_ins = create_gles1_ctx(env);

    let window = env.window.as_mut().expect("OpenGL ES is not supported in headless mode");
    {
        let gles1_ctx = gles1_ins.make_current(window);
        log!("Driver info: {}", unsafe { gles1_ctx.driver_description() });
    }

    env.objc.borrow_mut::<EAGLContextHostObject>(this).gles_ctx = Some(gles1_ins);

    env.window.as_mut().unwrap().set_share_with_current_context(false);

    env.objc.borrow_mut::<EAGLContextHostObject>(this).renderbuffer_drawable_bindings = env.objc.borrow::<EAGLContextHostObject>(group).renderbuffer_drawable_bindings.clone();
    this
}

- (id)initWithAPI:(EAGLRenderingAPI)api {
    if api != kEAGLRenderingAPIOpenGLES1 {
        log!(
            "TODO: App requested EAGL initWithAPI:{}, returning nil as we only support API 1 for now",
            api
        );
        return nil;
    }

    let mut gles1_ins = create_gles1_ctx(env);

    let window = env.window.as_mut().expect("OpenGL ES is not supported in headless mode");
    {
        let gles1_ctx = gles1_ins.make_current(window);
        log!("Driver info: {}", unsafe { gles1_ctx.driver_description() });
    }

    env.objc.borrow_mut::<EAGLContextHostObject>(this).gles_ctx = Some(gles1_ins);

    this
}

- (EAGLRenderingAPI)API {
    // TODO: support later API versions
    kEAGLRenderingAPIOpenGLES1
}

- (id)sharegroup {
    // We use object itself as the sharegroup.
    // Check initWithAPI:sharegroup: for more info
    this
}

- (())dealloc {
    let host_obj = env.objc.borrow_mut::<EAGLContextHostObject>(this);
    for &(guest_buf, _host_buf) in host_obj.mapped_buffers.values() {
        env.mem.free(guest_buf);
    }
    if Rc::strong_count(&host_obj.renderbuffer_drawable_bindings) == 1 {
        let bindings = std::mem::take(&mut host_obj.renderbuffer_drawable_bindings);
        for (_renderbuffer, drawable) in bindings.take() {
            release(env, drawable);
        }
    }
    env.objc.dealloc_object(this, &mut env.mem);
}

- (bool)renderbufferStorage:(NSUInteger)target
               fromDrawable:(id)drawable { // EAGLDrawable (always CAEAGLayer*)
    assert!(drawable != nil); // TODO: handle unbinding

    assert!(target == gles11::RENDERBUFFER_OES);

    let props: id = msg![env; drawable drawableProperties];

    let format_key = get_static_str(env, kEAGLDrawablePropertyColorFormat);
    let format_rgba8 = get_static_str(env, kEAGLColorFormatRGBA8);
    let format_rgb565 = get_static_str(env, kEAGLColorFormatRGB565);

    let format: id = msg![env; props objectForKey:format_key];
    // Theoretically this should map formats like:
    // - kColorFormatRGBA8 => RGBA8_OES
    // - kColorFormatRGB565 => RGB565_OES
    // However, the specification of EXT_framebuffer_object allows the
    // implementation to arbitrarily restrict which formats can be rendered to,
    // and it seems like RGB565 isn't supported, at least on a machine with
    // Intel HD Graphics 615 running macOS Monterey. I don't think RGBA8 is
    // guaranteed either, but it at least seems to work.
    if !msg![env; format isEqual:format_rgba8] && !msg![env; format isEqual:format_rgb565] {
        log!("[renderbufferStorage:{:?} fromDrawable:{:?}] Warning: unhandled format {:?}, using RGBA8", target, drawable, format);
    }
    let internalformat = gles11::RGBA8_OES;

    let (width, height) = {
        let bounds: CGRect = msg![env; drawable bounds];
        let CGSize { width, height } = bounds.size;
        assert!((0.0..(u32::MAX as f32)).contains(&width));
        assert!((0.0..(u32::MAX as f32)).contains(&height));
        let scale_hack = env.options.scale_hack.get();
        (width.round() as u32 * scale_hack, height.round() as u32 * scale_hack)
    };

    let window = env.window.as_mut().expect("OpenGL ES is not supported in headless mode");

    let renderbuffer = {
        // Unclear from documentation if this method requires an appropriate
        // context to already be active, but that seems to be the case
        // in practice?
        let mut gles = super::sync_context(&mut env.framework_state.opengles, &mut env.objc, window, env.current_thread);
        unsafe {
            gles.RenderbufferStorageOES(target, internalformat, width.try_into().unwrap(), height.try_into().unwrap());
            let mut renderbuffer = 0;
            gles.GetIntegerv(gles11::RENDERBUFFER_BINDING_OES, &mut renderbuffer);
            renderbuffer as _
        }
    };

    retain(env, drawable);
    let host_obj = env.objc.borrow_mut::<EAGLContextHostObject>(this);
    let maybe_old_drawable = host_obj.renderbuffer_drawable_bindings.borrow_mut().insert(
        renderbuffer,
        drawable
    );
    if let Some(old_drawable) = maybe_old_drawable {
        release(env, old_drawable);
    }

    true
}

- (bool)presentRenderbuffer:(NSUInteger)target {
    assert!(target == gles11::RENDERBUFFER_OES);

    // The presented frame should be displayed ASAP, but the next one must be
    // delayed, so this needs to be checked before returning.
    let sleep_for = limit_framerate(&mut env.objc.borrow_mut::<EAGLContextHostObject>(this).next_frame_due, &env.options);

    if env.options.print_fps {
        env
            .objc
            .borrow_mut::<EAGLContextHostObject>(this)
            .fps_counter
            .get_or_insert_with(FpsCounter::start)
            .count_frame(format_args!("EAGLContext {this:?}"));
    }

    let fullscreen_layer = find_fullscreen_eagl_layer(env);

    // Unclear from documentation if this method requires the context to be
    // current, but it would be weird if it didn't?
    let window = env.window.as_mut().expect("OpenGL ES is not supported in headless mode");
    let mut gles = super::sync_context(&mut env.framework_state.opengles, &mut env.objc, window, env.current_thread);

    let renderbuffer: GLuint = unsafe {
        let mut renderbuffer = 0;
        gles.GetIntegerv(gles11::RENDERBUFFER_BINDING_OES, &mut renderbuffer);
        renderbuffer as _
    };

    std::mem::drop(gles);

    let Some(&drawable) = env
        .objc
        .borrow::<EAGLContextHostObject>(this)
        .renderbuffer_drawable_bindings
        .borrow()
        .get(&renderbuffer) else {
        log_dbg!("Can't present a renderbuffer {:?} not bound to a drawable!", renderbuffer);
        return false;
    };

    // n72 fork: without kEAGLDrawablePropertyRetainedBacking (the default)
    // the renderbuffer's contents are undefined after presenting, as on iOS:
    // n72-gles is told so (glDiscardFramebufferEXT) once the frame is copied,
    // and the next frame skips reloading the old pixels on the GPU.
    let n72_discard = crate::window::Window::fixed_portrait_panel() && {
        n72_resolve_discard(env.window.as_ref().unwrap());
        !n72_retained_backing(env, drawable)
    };
    N72_DISCARD_NEXT.store(n72_discard, std::sync::atomic::Ordering::Relaxed);

    // We're presenting to the opaque CAEAGLLayer that covers the screen.
    // We can use the fast path where we skip composition and present directly.
    if drawable == fullscreen_layer {
        log_dbg!(
            "Layer {:?} is the fullscreen layer, presenting renderbuffer {:?} directly (fast path).",
            drawable,
            renderbuffer,
        );
        // n72 fork: once the renderbuffer is window-backed (n72-gles draws
        // it straight into the panel's back buffer), presenting is a swap.
        if crate::window::Window::fixed_portrait_panel() && n72_is_window_backed(renderbuffer) {
            drop(super::sync_context(&mut env.framework_state.opengles, &mut env.objc, env.window.as_mut().unwrap(), env.current_thread));
            env.window.as_ref().unwrap().swap_window();
            if let Some(sleep_for) = sleep_for {
                env.sleep(sleep_for);
            }
            return true;
        }
        // re-borrow
        unsafe {
            present_renderbuffer(env);
        }
        if crate::window::Window::fixed_portrait_panel() {
            let mut gles = super::sync_context(&mut env.framework_state.opengles, &mut env.objc, env.window.as_mut().unwrap(), env.current_thread);
            unsafe { n72_set_window_backed(gles.as_mut(), renderbuffer, true) };
        }
    } else {
        if crate::window::Window::fixed_portrait_panel() && n72_is_window_backed(renderbuffer) {
            // Not the only layer any more: back to the renderbuffer's own
            // storage (this frame is in the panel buffer: copy it out first
            // below, or drop it when this layer is hidden anyway).
            if fullscreen_layer != nil {
                let mut gles = super::sync_context(&mut env.framework_state.opengles, &mut env.objc, env.window.as_mut().unwrap(), env.current_thread);
                unsafe { n72_set_window_backed(gles.as_mut(), renderbuffer, false) };
            }
        }
        if fullscreen_layer != nil {
            // If there's a single layer that covers the screen, and this isn't
            // it, there's no point in presenting the output because it won't be
            // seen. Using a noisy log because it's a weird scenario and might
            // indicate a bug.
            log!(
                "Layer {:?} is not the fullscreen layer {:?}, skipping presentation of renderbuffer {:?}!",
                drawable,
                fullscreen_layer,
                renderbuffer,
            );
            if let Some(sleep_for) = sleep_for {
                env.sleep(sleep_for);
            }
            return true;
        }

        // The very slow and inefficient path: not only does glReadPixels()
        // block the thread until rendering finishes, but the result has to be
        // copied back to system RAM, and then will have to be copied to VRAM
        // again during composition. find_fullscreen_eagl_layer() exists to
        // avoid this.
        log_dbg!(
            "There is no fullscreen layer, presenting renderbuffer {:?} to layer {:?} by copying to RAM (slow path).",
            renderbuffer,
            drawable,
        );
        // n72 fork: on the device all GL contexts share objects with the
        // compositor's (gles.rs), so the frame is copied on the GPU into a
        // shared texture (n72-gles: MBX 2D copy) and never touches RAM.
        if crate::window::Window::fixed_portrait_panel() {
            let existing = crate::frameworks::core_animation::ca_eagl_layer::presented_texture_for(env, drawable);
            let opaque = crate::frameworks::core_animation::ca_eagl_layer::layer_is_opaque(env, drawable);
            let (texture, width, height) = {
                let mut gles = super::sync_context(&mut env.framework_state.opengles, &mut env.objc, env.window.as_mut().unwrap(), env.current_thread);
                let r = unsafe { copy_renderbuffer_to_texture(gles.as_mut(), existing, opaque) };
                if n72_is_window_backed(renderbuffer) {
                    unsafe { n72_set_window_backed(gles.as_mut(), renderbuffer, false) };
                }
                r
            };
            crate::frameworks::core_animation::ca_eagl_layer::present_texture(env, drawable, texture, width, height);
            if let Some(sleep_for) = sleep_for {
                env.sleep(sleep_for);
            }
            return true;
        }
        let pixels_vec = get_pixels_vec_for_presenting(env, drawable);
        // re-borrow
        let (pixels_vec, width, height) = {
            let mut gles = super::sync_context(&mut env.framework_state.opengles, &mut env.objc, env.window.as_mut().unwrap(), env.current_thread);
            unsafe {
                read_renderbuffer(gles.as_mut(), pixels_vec)
            }
        };
        present_pixels(env, drawable, pixels_vec, width, height);
    }

    if let Some(sleep_for) = sleep_for {
        env.sleep(sleep_for);
    }

    true
}

@end

};

/// Implement framerate limiting.
///
/// The real iPhone OS seems to force 60Hz v-sync in `presentRenderbuffer:`.
/// touchHLE does not force v-sync, and its users might not have 60Hz monitors
/// in any case, so to avoid excessive FPS or games running too fast, we need
/// to simulate it.
///
/// V-sync is essentially a limiter with no "slop", or allowance for frames
/// arriving late: if the frame misses a 60Hz interval, it must wait until the
/// next one. This is quite harsh: if frames consistently arrive very slightly
/// late, the framerate is halved!
///
/// Most games already use NSTimer, which is itself a v-sync-like limiter.
/// For the remainder, let's do something a bit kinder, for the benefit of users
/// with slow systems or which are using high scale hack settings: allow at most
/// an interval's worth of accumulated slop. Allowing infinite accumulation of
/// slop is not desirable, because if the game is running slowly for a long time
/// and suddenly speeds back up, it will then run too fast for a long time.
fn limit_framerate(next_frame_due: &mut Option<Instant>, options: &Options) -> Option<Duration> {
    let interval = {
        let fps = options.fps_limit?;
        1.0 / fps
    };
    let interval_rust = Duration::from_secs_f64(interval);

    let &mut Some(current_frame_due) = next_frame_due else {
        // First frame presented: no delay yet.
        *next_frame_due = Some(Instant::now() + interval_rust);
        return None;
    };

    let now = Instant::now();
    *next_frame_due = if now > current_frame_due + interval_rust {
        // Too much slop has accumulated. Make the next frame wait for the next
        // interval.
        log_dbg!("Too much slop accumulated, skipping an interval.");
        Some(
            current_frame_due
                + Duration::from_secs_f64(
                    interval * (((now - current_frame_due).as_secs_f64() / interval).ceil()),
                ),
        )
    } else {
        // Time next frame based on when the current frame was due, not
        // the current time, so as to allow some slop.
        Some(current_frame_due + interval_rust)
    };

    if now < current_frame_due {
        // Frame was presented early, delay it to maintain framerate limit.
        Some(current_frame_due.saturating_duration_since(now))
    } else {
        // Frame was presented on time or late, don't delay.
        None
    }
}

// These helper functions make the state backup code easier to read, but
// more importantly, they make it free of mutable variables that wouldn't
// get caught by Rust's unused variable warnings, which are useful to check
// we actually restore the stuff we back up.

unsafe fn get_ptr(gles: &mut dyn GLES, pname: GLenum) -> *const GLvoid {
    let mut ptr = std::ptr::null();
    gles.GetPointerv(pname, &mut ptr);
    ptr
}
// Safety: caller's responsibility to use appropriate N.
unsafe fn get_ints<const N: usize>(gles: &mut dyn GLES, pname: GLenum) -> [GLint; N] {
    let mut res = [0; N];
    gles.GetIntegerv(pname, res.as_mut_ptr());
    res
}
// Safety: caller's responsibility to only use this for scalars.
unsafe fn get_int(gles: &mut dyn GLES, pname: GLenum) -> GLint {
    get_ints::<1>(gles, pname)[0]
}
// Safety: caller's responsibility to use appropriate N.
unsafe fn get_tex_env_ints<const N: usize>(
    gles: &mut dyn GLES,
    target: GLenum,
    pname: GLenum,
) -> [GLint; N] {
    let mut res = [0; N];
    gles.GetTexEnviv(target, pname, res.as_mut_ptr());
    res
}
// Safety: caller's responsibility to only use this for scalars.
unsafe fn get_tex_env_int(gles: &mut dyn GLES, target: GLenum, pname: GLenum) -> GLint {
    get_tex_env_ints::<1>(gles, target, pname)[0]
}
// Safety: caller's responsibility to use appropriate N.
unsafe fn get_floats<const N: usize>(gles: &mut dyn GLES, pname: GLenum) -> [GLfloat; N] {
    let mut res = [0.0; N];
    gles.GetFloatv(pname, res.as_mut_ptr());
    res
}
unsafe fn get_renderbuffer_size(gles: &mut dyn GLES) -> (GLsizei, GLsizei) {
    let mut width: GLint = 0;
    let mut height: GLint = 0;
    gles.GetRenderbufferParameterivOES(
        gles11::RENDERBUFFER_OES,
        gles11::RENDERBUFFER_WIDTH_OES,
        &mut width,
    );
    gles.GetRenderbufferParameterivOES(
        gles11::RENDERBUFFER_OES,
        gles11::RENDERBUFFER_HEIGHT_OES,
        &mut height,
    );
    (width, height)
}

/// Copies the pixels in a renderbuffer bound to `GL_RENDERBUFFER_BINDING_OES`
/// (which should be provided by the app) to a provided [Vec], trying to avoid
/// noticeably modifying OpenGL ES state while doing so.
///
/// This uses `glReadPixels()`, with all the associated performance risks. Any
/// existing content in the [Vec] will bereplaced. The format is RGBA8.
/// The returned values are the [Vec], the width and height.
///
/// The provided context must be current.
unsafe fn read_renderbuffer(gles: &mut dyn GLES, mut pixel_buffer: Vec<u8>) -> (Vec<u8>, u32, u32) {
    let renderbuffer: GLuint = get_int(gles, gles11::RENDERBUFFER_BINDING_OES) as _;
    let (width, height) = get_renderbuffer_size(gles);
    let width_u32: u32 = width.try_into().unwrap();
    let height_u32: u32 = height.try_into().unwrap();

    // To avoid confusing the guest app, we need to be able to undo any
    // state changes we make.
    let old_framebuffer: GLuint = get_int(gles, gles11::FRAMEBUFFER_BINDING_OES) as _;

    // Create a framebuffer we can use to read from the renderbuffer
    let mut src_framebuffer = 0;
    gles.GenFramebuffersOES(1, &mut src_framebuffer);
    gles.BindFramebufferOES(gles11::FRAMEBUFFER_OES, src_framebuffer);
    gles.FramebufferRenderbufferOES(
        gles11::FRAMEBUFFER_OES,
        gles11::COLOR_ATTACHMENT0_OES,
        gles11::RENDERBUFFER_OES,
        renderbuffer,
    );

    // Read the pixels
    let size = (width_u32 as usize)
        .checked_mul(height_u32 as usize)
        .unwrap()
        .checked_mul(4)
        .unwrap();
    pixel_buffer.clear();
    pixel_buffer.reserve_exact(size);
    let before = Instant::now();
    gles.ReadPixels(
        0,
        0,
        width,
        height,
        gles11::RGBA,
        gles11::UNSIGNED_BYTE,
        pixel_buffer.as_mut_ptr() as *mut _,
    );
    log_dbg!(
        "glReadPixels(0, 0, {}, {}, …) took {:?}",
        width,
        height,
        Instant::now().saturating_duration_since(before)
    );
    pixel_buffer.set_len(size);

    // Clean up the framebuffer object since we no longer need it.
    gles.DeleteFramebuffersOES(1, &src_framebuffer);

    // Restore the framebuffer binding
    gles.BindFramebufferOES(gles11::FRAMEBUFFER_OES, old_framebuffer);

    (pixel_buffer, width_u32, height_u32)
}

/// n72 fork: renderbuffers n72-gles draws straight into the panel's back
/// buffer (`glRenderbufferWindowBackedN72`). Names are per context; the
/// device panel only runs apps with one rendering context in practice, and a
/// name from another context just takes the normal path.
static N72_WINDOW_BACKED: std::sync::Mutex<Vec<GLuint>> = std::sync::Mutex::new(Vec::new());
static N72_WINDOW_BACKED_FN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

fn n72_is_window_backed(renderbuffer: GLuint) -> bool {
    N72_WINDOW_BACKED.lock().unwrap().contains(&renderbuffer)
}

unsafe fn n72_set_window_backed(gles: &mut dyn GLES, renderbuffer: GLuint, on: bool) {
    if std::env::var_os("N72_WINDOW_BACKED").is_some_and(|v| v == "0") {
        return;
    }
    let f = N72_WINDOW_BACKED_FN.load(std::sync::atomic::Ordering::Relaxed);
    if f <= 1 {
        return;
    }
    let set: extern "C" fn(GLuint, u8) = std::mem::transmute(f);
    // Keep the app's error state: report only our own call's result.
    let pending = gles.GetError();
    set(renderbuffer, on as u8);
    let ok = gles.GetError() == gles11::NO_ERROR;
    let mut list = N72_WINDOW_BACKED.lock().unwrap();
    list.retain(|&r| r != renderbuffer);
    if on && ok {
        list.push(renderbuffer);
        log!("n72: renderbuffer {} is window-backed (presents are swaps)", renderbuffer);
    }
    let _ = pending;
}

/// n72 fork: whether the current present may discard the renderbuffer.
static N72_DISCARD_NEXT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// n72 fork: `glDiscardFramebufferEXT` from the host GL (0: not offered).
static N72_DISCARD_FN: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(1);

fn n72_resolve_discard(window: &crate::window::Window) {
    if N72_DISCARD_FN.load(std::sync::atomic::Ordering::Relaxed) == 1 {
        let f = window.gl_get_proc_address("glDiscardFramebufferEXT");
        N72_DISCARD_FN.store(f as usize, std::sync::atomic::Ordering::Relaxed);
        let f = window.gl_get_proc_address("glRenderbufferWindowBackedN72");
        N72_WINDOW_BACKED_FN.store(f as usize, std::sync::atomic::Ordering::Relaxed);
    }
}

fn n72_retained_backing(env: &mut Environment, drawable: id) -> bool {
    let props: id = msg![env; drawable drawableProperties];
    if props == nil {
        return false;
    }
    let key = get_static_str(env, kEAGLDrawablePropertyRetainedBacking);
    let value: id = msg![env; props objectForKey:key];
    value != nil && msg![env; value boolValue]
}

/// n72 fork: the colour attachment of the bound framebuffer (the app's
/// renderbuffer, just copied out) is no longer needed.
unsafe fn n72_discard_bound_color() {
    if !N72_DISCARD_NEXT.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let f = N72_DISCARD_FN.load(std::sync::atomic::Ordering::Relaxed);
    if f > 1 {
        let discard: extern "C" fn(GLenum, i32, *const GLenum) = std::mem::transmute(f);
        let attachments = [gles11::COLOR_ATTACHMENT0_OES];
        discard(gles11::FRAMEBUFFER_OES, 1, attachments.as_ptr());
    }
}

/// n72 fork: copy the renderbuffer bound to `GL_RENDERBUFFER_BINDING_OES` into
/// `texture` (or a new texture) with glCopyTexImage2D, restoring the app's
/// bindings. Returns the texture and the frame size.
unsafe fn copy_renderbuffer_to_texture(
    gles: &mut dyn GLES,
    texture: Option<GLuint>,
    opaque: bool,
) -> (GLuint, u32, u32) {
    let renderbuffer: GLuint = get_int(gles, gles11::RENDERBUFFER_BINDING_OES) as _;
    let (width, height) = get_renderbuffer_size(gles);
    let old_framebuffer: GLuint = get_int(gles, gles11::FRAMEBUFFER_BINDING_OES) as _;
    let old_active_texture: GLenum = get_int(gles, gles11::ACTIVE_TEXTURE) as _;
    gles.ActiveTexture(gles11::TEXTURE0);
    let old_texture: GLuint = get_int(gles, gles11::TEXTURE_BINDING_2D) as _;

    let mut src_framebuffer = 0;
    gles.GenFramebuffersOES(1, &mut src_framebuffer);
    gles.BindFramebufferOES(gles11::FRAMEBUFFER_OES, src_framebuffer);
    gles.FramebufferRenderbufferOES(
        gles11::FRAMEBUFFER_OES,
        gles11::COLOR_ATTACHMENT0_OES,
        gles11::RENDERBUFFER_OES,
        renderbuffer,
    );

    let texture = texture.unwrap_or_else(|| {
        let mut t = 0;
        gles.GenTextures(1, &mut t);
        t
    });
    gles.BindTexture(gles11::TEXTURE_2D, texture);
    // Composited 1:1 on the device panel: NEAREST is the same image and half
    // the MBX's cost for a linear-layout texture (see present_renderbuffer).
    gles.TexParameteri(
        gles11::TEXTURE_2D,
        gles11::TEXTURE_MIN_FILTER,
        gles11::NEAREST as _,
    );
    gles.TexParameteri(
        gles11::TEXTURE_2D,
        gles11::TEXTURE_MAG_FILTER,
        gles11::NEAREST as _,
    );
    gles.TexParameteri(
        gles11::TEXTURE_2D,
        gles11::TEXTURE_WRAP_S,
        gles11::CLAMP_TO_EDGE as _,
    );
    gles.TexParameteri(
        gles11::TEXTURE_2D,
        gles11::TEXTURE_WRAP_T,
        gles11::CLAMP_TO_EDGE as _,
    );
    // RGB for an opaque layer: its alpha is meant to be ignored.
    let format = if opaque { gles11::RGB } else { gles11::RGBA };
    gles.CopyTexImage2D(gles11::TEXTURE_2D, 0, format, 0, 0, width, height, 0);
    n72_discard_bound_color();

    gles.BindTexture(gles11::TEXTURE_2D, old_texture);
    gles.ActiveTexture(old_active_texture);
    gles.DeleteFramebuffersOES(1, &src_framebuffer);
    gles.BindFramebufferOES(gles11::FRAMEBUFFER_OES, old_framebuffer);
    (texture, width as u32, height as u32)
}

/// Copies the pixels in a renderbuffer bound to `GL_RENDERBUFFER_BINDING_OES`
/// (which should be provided by the app) to a texture and presents it with
/// [present_frame], trying to avoid noticeably modifying OpenGL ES state while
/// doing so. The front and back buffers are then swapped.
unsafe fn present_renderbuffer(env: &mut Environment) {
    // Save these for when we need to draw the frame
    let viewport = env.window.as_mut().unwrap().viewport();
    let rotation_matrix = env.window.as_mut().unwrap().rotation_matrix();
    let virtual_cursor_visible_at = env.window.as_mut().unwrap().virtual_cursor_visible_at();

    let gles_ctx = super::get_thread_context(
        &mut env.framework_state.opengles,
        &mut env.objc,
        env.current_thread,
    );

    let mut gles_boxed = gles_ctx.make_current(env.window.as_mut().unwrap());
    let gles = gles_boxed.as_mut();

    // We can't directly copy the content of the renderbuffer to the default
    // framebuffer (the window), but if we attach it to a framebuffer object, we
    // can use glCopyTexImage2D() to copy it to a texture, which we can then
    // draw to the default framebuffer via a textured quad, which can be
    // rotated, scaled or letterboxed as appropriate.

    let renderbuffer: GLuint = get_int(gles, gles11::RENDERBUFFER_BINDING_OES) as _;
    let (width, height) = get_renderbuffer_size(gles);

    // To avoid confusing the guest app, we need to be able to undo any
    // state changes we make.
    let old_framebuffer: GLuint = get_int(gles, gles11::FRAMEBUFFER_BINDING_OES) as _;
    let old_texture_2d: GLuint = get_int(gles, gles11::TEXTURE_BINDING_2D) as _;

    // Create a framebuffer we can use to read from the renderbuffer
    let mut src_framebuffer = 0;
    gles.GenFramebuffersOES(1, &mut src_framebuffer);
    gles.BindFramebufferOES(gles11::FRAMEBUFFER_OES, src_framebuffer);
    gles.FramebufferRenderbufferOES(
        gles11::FRAMEBUFFER_OES,
        gles11::COLOR_ATTACHMENT0_OES,
        gles11::RENDERBUFFER_OES,
        renderbuffer,
    );

    // Create a texture with a copy of the pixels in the framebuffer
    // n72 fork: on the device panel keep one present texture (all contexts
    // share objects there) instead of a new one per frame: n72-gles gives a
    // texture copied into on the GPU linear render-target storage, which is
    // expensive to set up and free every frame.
    static N72_PRESENT_TEXTURE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let keep_texture = crate::window::Window::fixed_portrait_panel();
    let mut texture: GLuint = if keep_texture {
        N72_PRESENT_TEXTURE.load(std::sync::atomic::Ordering::Relaxed)
    } else {
        0
    };
    if texture == 0 || gles.IsTexture(texture) == 0 {
        gles.GenTextures(1, &mut texture);
        if keep_texture {
            N72_PRESENT_TEXTURE.store(texture, std::sync::atomic::Ordering::Relaxed);
        }
    }
    gles.BindTexture(gles11::TEXTURE_2D, texture);
    gles.CopyTexImage2D(
        gles11::TEXTURE_2D,
        0,
        gles11::RGB as _,
        0,
        0,
        width,
        height,
        0,
    );
    n72_discard_bound_color();
    // The texture will not have any mip levels so we must ensure the filter
    // does not use them, else rendering will fail.
    // n72 fork: on the device panel it is drawn 1:1, where NEAREST gives the
    // same image and the MBX fetches a linear-layout texture twice as fast.
    let n72_filter = if crate::window::Window::fixed_portrait_panel() {
        gles11::NEAREST
    } else {
        gles11::LINEAR
    };
    gles.TexParameteri(gles11::TEXTURE_2D, gles11::TEXTURE_MIN_FILTER, n72_filter as _);
    gles.TexParameteri(gles11::TEXTURE_2D, gles11::TEXTURE_MAG_FILTER, n72_filter as _);

    // Clean up the framebuffer object since we no longer need it.
    // This also sets the framebuffer bindings back to zero, so rendering
    // will go to the default framebuffer (the window).
    gles.DeleteFramebuffersOES(1, &src_framebuffer);

    // Reset various things that could affect the quad or virtual cursor we're
    // going to draw. Back up the old state while doing so, so it can be
    // restored later. The app's subsequent drawing will be messed up if we
    // don't restore it.
    let old_arrays = {
        let mut old_arrays = [gles11::FALSE; gles1_on_gl2::ARRAYS.len()];
        for (is_enabled, info) in old_arrays.iter_mut().zip(gles1_on_gl2::ARRAYS.iter()) {
            gles.GetBooleanv(info.name, is_enabled);
            gles.DisableClientState(info.name);
        }
        old_arrays
    };
    let old_capabilities = {
        let mut old_capabilities = [gles11::FALSE; gles1_on_gl2::CAPABILITIES.len()];
        for (is_enabled, &name) in old_capabilities
            .iter_mut()
            .zip(gles1_on_gl2::CAPABILITIES.iter())
        {
            gles.GetBooleanv(name, is_enabled);
            gles.Disable(name);
        }
        old_capabilities
    };
    let old_matrix_mode: GLenum = get_int(gles, gles11::MATRIX_MODE) as _;
    for mode in [gles11::MODELVIEW, gles11::PROJECTION, gles11::TEXTURE] {
        gles.MatrixMode(mode);
        gles.PushMatrix();
        gles.LoadIdentity();
    }
    let old_color: [GLfloat; 4] = get_floats(gles, gles11::CURRENT_COLOR);
    gles.Color4f(1.0, 1.0, 1.0, 1.0);

    // Back up other things that will be modified while drawing.
    let old_viewport: (GLint, GLint, GLsizei, GLsizei) = {
        let [x, y, width, height] = get_ints(gles, gles11::VIEWPORT);
        (x, y, width as _, height as _)
    };
    let old_clear_color: [GLfloat; 4] = get_floats(gles, gles11::COLOR_CLEAR_VALUE);
    let old_array_buffer: GLuint = get_int(gles, gles11::ARRAY_BUFFER_BINDING) as _;
    let old_vertex_array_binding: GLuint = get_int(gles, gles11::VERTEX_ARRAY_BUFFER_BINDING) as _;
    let old_vertex_array_size: GLint = get_int(gles, gles11::VERTEX_ARRAY_SIZE);
    let old_vertex_array_type: GLenum = get_int(gles, gles11::VERTEX_ARRAY_TYPE) as _;
    let old_vertex_array_stride: GLsizei = get_int(gles, gles11::VERTEX_ARRAY_STRIDE) as _;
    let old_vertex_array_pointer = get_ptr(gles, gles11::VERTEX_ARRAY_POINTER);
    let old_tex_coord_array_binding: GLuint =
        get_int(gles, gles11::TEXTURE_COORD_ARRAY_BUFFER_BINDING) as _;
    let old_tex_coord_array_size: GLint = get_int(gles, gles11::TEXTURE_COORD_ARRAY_SIZE);
    let old_tex_coord_array_type: GLenum = get_int(gles, gles11::TEXTURE_COORD_ARRAY_TYPE) as _;
    let old_tex_coord_array_stride: GLsizei =
        get_int(gles, gles11::TEXTURE_COORD_ARRAY_STRIDE) as _;
    let old_tex_coord_array_pointer = get_ptr(gles, gles11::TEXTURE_COORD_ARRAY_POINTER);
    let old_blend_sfactor: GLenum = get_int(gles, gles11::BLEND_SRC) as _;
    let old_blend_dfactor: GLenum = get_int(gles, gles11::BLEND_DST) as _;

    let old_tex_env_mode = get_tex_env_int(gles, gles11::TEXTURE_ENV, gles11::TEXTURE_ENV_MODE);
    // if the mode is REPLACE, we don't have to reset the other texture
    // environment values
    let tex_env_mode_arr = [gles11::REPLACE; 1];
    gles.TexEnviv(
        gles11::TEXTURE_ENV,
        gles11::TEXTURE_ENV_MODE,
        tex_env_mode_arr.as_ptr().cast(),
    );

    // Draw the quad
    present_frame(gles, viewport, rotation_matrix, virtual_cursor_visible_at);

    // Clean up the texture (kept for the next frame on the device panel)
    if !keep_texture {
        gles.DeleteTextures(1, &texture);
    }

    // Restore all the state saved before rendering
    for (&is_enabled, info) in old_arrays.iter().zip(gles1_on_gl2::ARRAYS.iter()) {
        match is_enabled {
            gles11::TRUE => gles.EnableClientState(info.name),
            gles11::FALSE => gles.DisableClientState(info.name),
            _ => unreachable!(),
        }
    }
    for (&is_enabled, &name) in old_capabilities
        .iter()
        .zip(gles1_on_gl2::CAPABILITIES.iter())
    {
        match is_enabled {
            gles11::TRUE => gles.Enable(name),
            gles11::FALSE => gles.Disable(name),
            _ => unreachable!(),
        }
    }
    for mode in [gles11::MODELVIEW, gles11::PROJECTION, gles11::TEXTURE] {
        gles.MatrixMode(mode);
        gles.PopMatrix();
    }
    gles.MatrixMode(old_matrix_mode);
    gles.Color4f(old_color[0], old_color[1], old_color[2], old_color[3]);
    gles.Viewport(
        old_viewport.0,
        old_viewport.1,
        old_viewport.2,
        old_viewport.3,
    );
    gles.ClearColor(
        old_clear_color[0],
        old_clear_color[1],
        old_clear_color[2],
        old_clear_color[3],
    );
    // GL_ARRAY_BUFFER is implicitly used by the Pointer functions but is also
    // an independent binding.
    gles.BindBuffer(gles11::ARRAY_BUFFER, old_vertex_array_binding);
    gles.VertexPointer(
        old_vertex_array_size,
        old_vertex_array_type,
        old_vertex_array_stride,
        old_vertex_array_pointer,
    );
    gles.BindBuffer(gles11::ARRAY_BUFFER, old_tex_coord_array_binding);
    gles.TexCoordPointer(
        old_tex_coord_array_size,
        old_tex_coord_array_type,
        old_tex_coord_array_stride,
        old_tex_coord_array_pointer,
    );
    gles.BindBuffer(gles11::ARRAY_BUFFER, old_array_buffer);
    gles.BlendFunc(old_blend_sfactor, old_blend_dfactor);

    let old_tex_env_mode_arr = [old_tex_env_mode; 1];
    gles.TexEnviv(
        gles11::TEXTURE_ENV,
        gles11::TEXTURE_ENV_MODE,
        old_tex_env_mode_arr.as_ptr().cast(),
    );

    std::mem::drop(gles_boxed);

    // SDL2's documentation warns 0 should be bound to the draw framebuffer
    // when swapping the window, so this is the perfect moment.
    env.window.as_ref().unwrap().swap_window();

    let mut gles_boxed = gles_ctx.make_current(env.window.as_mut().unwrap());
    let gles = gles_boxed.as_mut();

    // Restore the other bindings
    gles.BindTexture(gles11::TEXTURE_2D, old_texture_2d);
    gles.BindFramebufferOES(gles11::FRAMEBUFFER_OES, old_framebuffer);

    // { let err = gles.GetError(); if err != 0 { panic!("{:#x}", err); } }
}
