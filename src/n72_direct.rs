/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! n72 fork: direct calls from guest code into host GL and libm.
//!
//! With the native CPU backend guest pointers are host pointers, so the
//! hottest OpenGL ES imports (pure pass-throughs in `gles_guest`) and a few
//! libm functions can be called straight from guest code: the stub branches
//! to a trampoline that calls n72-gles (or a soft-float shim) without the
//! gate, the context lookup and the boxed context per call. Spore makes
//! ~1700 such calls per frame.
//!
//! The trampoline only goes direct while [READY] says the current host GL
//! context is the running guest thread's: it is set after `sync_context`
//! made that context current, and cleared whenever any context is made
//! current and on every guest thread switch. Otherwise the call takes the
//! gate (which makes the right context current and sets [READY] again).
//!
//! Disable with `N72_DIRECT=0`.

use std::collections::HashMap;
use std::sync::OnceLock;

#[cfg(feature = "cpu-native")]
use crate::cpu::N72_GL_DIRECT_READY as READY_FLAG;

/// Address of the ready flag the trampolines test.
#[cfg(feature = "cpu-native")]
fn ready_addr() -> u32 {
    std::ptr::addr_of!(READY_FLAG) as u32
}

pub fn enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        cfg!(feature = "cpu-native")
            && !std::env::var_os("N72_DIRECT").is_some_and(|v| v == "0")
            && crate::window::Window::fixed_portrait_panel()
    })
}

/// The current host GL context is (`true`) or may not be (`false`) the
/// running guest thread's.
#[inline]
pub fn set_ready(ready: bool) {
    #[cfg(feature = "cpu-native")]
    unsafe {
        std::ptr::addr_of_mut!(READY_FLAG).write_volatile(ready as u32);
    }
    #[cfg(not(feature = "cpu-native"))]
    let _ = ready;
}

/// Host GL entry points (hard-float), by GL name.
static GL: OnceLock<HashMap<&'static str, usize>> = OnceLock::new();

/// Functions with only integer/pointer arguments: called as they are.
const DIRECT_GL: &[&str] = &[
    "glEnableClientState",
    "glDisableClientState",
    "glMatrixMode",
    "glPushMatrix",
    "glPopMatrix",
    "glLoadIdentity",
    "glLoadMatrixf",
    "glMultMatrixf",
    "glBlendFunc",
    "glVertexPointer",
    "glTexCoordPointer",
    "glColorPointer",
    "glNormalPointer",
    "glDrawArrays",
    "glBindTexture",
    "glEnable",
    "glDisable",
    "glLightfv",
    // Fixed-point API (Galaxy on Fire: ~600 glLoadMatrixx and as many
    // glDrawElements per frame, a quad each). With the native CPU backend an
    // index pointer needs no translation, and the fog clamp in `gles_guest`
    // works around other drivers' bugs, not n72-gles's (glDrawArrays already
    // skips it here).
    "glLoadMatrixx",
    "glMultMatrixx",
    "glDrawElements",
    "glDepthMask",
    "glColor4x",
    "glColor4ub",
    "glShadeModel",
    "glScalex",
    "glTranslatex",
    "glRotatex",
    "glTexEnvx",
    "glCullFace",
    "glDepthFunc",
    "glFrontFace",
];

/// Functions with float arguments: through a soft-float shim below.
const SHIM_GL: &[&str] = &[
    "glRotatef",
    "glTranslatef",
    "glScalef",
    "glColor4f",
    "glOrthof",
    "glMaterialf",
    // Not float, but the face needs mapping ([crate::gles::apple_material_face]).
    "glMaterialfv",
    "glMaterialx",
    "glMaterialxv",
];

/// Resolve the host GL functions once a GL context exists.
pub fn resolve_gl(window: &crate::window::Window) {
    if !enabled() || GL.get().is_some() {
        return;
    }
    let mut map = HashMap::new();
    for &name in DIRECT_GL.iter().chain(SHIM_GL.iter()) {
        let f = window.gl_get_proc_address(name) as usize;
        if f != 0 {
            map.insert(name, f);
        }
    }
    log!("n72: direct GL calls for {} functions", map.len());
    let _ = GL.set(map);
}

fn gl(name: &str) -> usize {
    GL.get().and_then(|m| m.get(name).copied()).unwrap_or(0)
}

macro_rules! shims {
    ($($name:ident ( $($arg:ident : $ty:ty),* ) ;)*) => {
        $(
            #[allow(non_snake_case)]
            extern "aapcs" fn $name($($arg: $ty),*) {
                static F: OnceLock<usize> = OnceLock::new();
                let f = *F.get_or_init(|| gl(stringify!($name)));
                let f: extern "C" fn($($ty),*) = unsafe { std::mem::transmute(f) };
                f($($arg),*)
            }
        )*
        fn shim_for(name: &str) -> Option<usize> {
            match name {
                $(stringify!($name) => Some($name as usize),)*
                "glOrthof" => Some(glOrthof as usize),
                _ => None,
            }
        }
    };
}

shims! {
    glRotatef(a: f32, x: f32, y: f32, z: f32);
    glTranslatef(x: f32, y: f32, z: f32);
    glScalef(x: f32, y: f32, z: f32);
    glColor4f(r: f32, g: f32, b: f32, a: f32);
}

/// glMaterial*: the face goes through [crate::gles::apple_material_face]
/// first (GL_FRONT is taken, as on iPhone OS; n72-gles alone would raise
/// GL_INVALID_ENUM, which stays pending for the app's next glGetError).
macro_rules! material_shims {
    ($($name:ident ( $param:ty ) ;)*) => {
        $(
            #[allow(non_snake_case)]
            extern "aapcs" fn $name(face: u32, pname: u32, param: $param) {
                static F: OnceLock<usize> = OnceLock::new();
                let f = *F.get_or_init(|| gl(stringify!($name)));
                let f: extern "C" fn(u32, u32, $param) = unsafe { std::mem::transmute(f) };
                if let Some(face) = crate::gles::apple_material_face(face) {
                    f(face, pname, param)
                }
            }
        )*
        fn material_shim_for(name: &str) -> Option<usize> {
            match name {
                $(stringify!($name) => Some($name as usize),)*
                _ => None,
            }
        }
    };
}

material_shims! {
    glMaterialf(f32);
    glMaterialfv(*const f32);
    glMaterialx(i32);
    glMaterialxv(*const i32);
}

/// glOrthof has six words of arguments: the last two are on the guest's
/// stack, above the two words the direct stub's routine pushed
/// ([crate::cpu::write_direct]).
#[allow(non_snake_case, clippy::too_many_arguments)]
extern "aapcs" fn glOrthof(l: f32, r: f32, b: f32, t: f32, _pushed_r4: u32, _pushed_lr: u32, n: f32, f: f32) {
    static F: OnceLock<usize> = OnceLock::new();
    let host = *F.get_or_init(|| gl("glOrthof"));
    let host: extern "C" fn(f32, f32, f32, f32, f32, f32) = unsafe { std::mem::transmute(host) };
    host(l, r, b, t, n, f)
}

extern "aapcs" fn n72_sinf(x: f32) -> f32 {
    x.sin()
}
extern "aapcs" fn n72_cosf(x: f32) -> f32 {
    x.cos()
}
extern "aapcs" fn n72_sqrtf(x: f32) -> f32 {
    x.sqrt()
}
extern "aapcs" fn n72_atanf(x: f32) -> f32 {
    x.atan()
}
extern "aapcs" fn n72_atan2f(y: f32, x: f32) -> f32 {
    y.atan2(x)
}
extern "aapcs" fn n72_fabsf(x: f32) -> f32 {
    x.abs()
}

/// The host function (guest calling convention) for an import, and whether
/// it needs the GL context ready.
fn target(symbol: &str) -> Option<(usize, bool)> {
    let name = symbol.strip_prefix('_')?;
    match name {
        "sinf" => return Some((n72_sinf as usize, false)),
        "cosf" => return Some((n72_cosf as usize, false)),
        "sqrtf" => return Some((n72_sqrtf as usize, false)),
        "atanf" => return Some((n72_atanf as usize, false)),
        "atan2f" => return Some((n72_atan2f as usize, false)),
        "fabsf" => return Some((n72_fabsf as usize, false)),
        _ => (),
    }
    if DIRECT_GL.contains(&name) {
        let f = gl(name);
        return (f != 0).then_some((f, true));
    }
    if SHIM_GL.contains(&name) && gl(name) != 0 {
        return shim_for(name).or_else(|| material_shim_for(name)).map(|f| (f, true));
    }
    None
}

/// Called by dyld after linking `symbol` at `site` to a host function's gate.
pub fn link(mem: &mut crate::mem::Mem, symbol: &str, site: u32) {
    if !enabled() {
        return;
    }
    #[cfg(feature = "cpu-native")]
    if let Some((f, needs_gl)) = target(symbol) {
        // libm needs no context: a flag that is always set.
        static ALWAYS: u32 = 1;
        let ready = if needs_gl {
            ready_addr()
        } else {
            std::ptr::addr_of!(ALWAYS) as u32
        };
        if crate::cpu::write_direct(mem, site, f as u32, ready) {
            log_dbg!("n72: {} at {:#x} calls the host directly", symbol, site);
        }
    }
    #[cfg(not(feature = "cpu-native"))]
    let _ = (mem, symbol, site, target);
}
