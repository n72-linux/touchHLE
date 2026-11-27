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
    *ON.get_or_init(|| cfg!(feature = "cpu-native") && crate::window::Window::fixed_portrait_panel())
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
    "glTexEnvi",
    "glCullFace",
    "glDepthFunc",
    "glFrontFace",
    "glClear",
    "glClearColorx",
    "glClearDepthx",
    "glAlphaFuncx",
    "glActiveTexture",
    "glClientActiveTexture",
    "glBindBuffer",
    "glColorMask",
    "glLightx",
    "glLightxv",
    "glFogx",
    "glFogxv",
    "glFogfv",
    "glNormal3x",
    "glPointSizex",
    "glLineWidthx",
    "glPolygonOffsetx",
    "glDepthRangex",
    "glHint",
    "glLightModelx",
    "glLightModelxv",
    "glLightModelfv",
    "glTexEnvfv",
    "glTexEnvxv",
    "glTexEnviv",
    "glPixelStorei",
];

/// Direct GL functions that assume `scale_hack == 1` (resolved only then).
const DIRECT_GL_UNSCALED: &[&str] = &["glScissor", "glViewport"];

/// Functions with float arguments or minor Apple/GLES shims.
const SHIM_GL: &[&str] = &[
    "glRotatef",
    "glTranslatef",
    "glScalef",
    "glColor4f",
    "glClearColor",
    "glClearDepthf",
    "glAlphaFunc",
    "glTexEnvf",
    "glFogf",
    "glLightf",
    "glDepthRangef",
    "glPolygonOffset",
    "glNormal3f",
    "glLineWidth",
    "glPointSize",
    "glMaterialf",
    // Not float, but the face needs mapping ([crate::gles::apple_material_face]).
    "glMaterialfv",
    "glMaterialx",
    "glMaterialxv",
    // Ignore GL_TEXTURE_CROP_RECT_OES like gles_guest.
    "glTexParameteri",
    "glTexParameterx",
    "glTexParameterf",
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
    if window.scale_hack().get() == 1 {
        for &name in DIRECT_GL_UNSCALED {
            let f = window.gl_get_proc_address(name) as usize;
            if f != 0 {
                map.insert(name, f);
            }
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
                $(stringify!($name) => Some($name as *const () as usize),)*
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
    glClearColor(r: f32, g: f32, b: f32, a: f32);
    glClearDepthf(d: f32);
    glAlphaFunc(func: u32, ref_: f32);
    glTexEnvf(target: u32, pname: u32, param: f32);
    glFogf(pname: u32, param: f32);
    glLightf(light: u32, pname: u32, param: f32);
    glDepthRangef(near: f32, far: f32);
    glPolygonOffset(factor: f32, units: f32);
    glNormal3f(nx: f32, ny: f32, nz: f32);
    glLineWidth(val: f32);
    glPointSize(size: f32);
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
                $(stringify!($name) => Some($name as *const () as usize),)*
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

/// glTexParameter*: ignore GL_TEXTURE_CROP_RECT_OES like gles_guest.
macro_rules! tex_param_shims {
    ($($name:ident ( $param:ty ) ;)*) => {
        $(
            #[allow(non_snake_case)]
            extern "aapcs" fn $name(target: u32, pname: u32, param: $param) {
                if pname == crate::gles::gles11_raw::TEXTURE_CROP_RECT_OES {
                    return;
                }
                static F: OnceLock<usize> = OnceLock::new();
                let f = *F.get_or_init(|| gl(stringify!($name)));
                let f: extern "C" fn(u32, u32, $param) = unsafe { std::mem::transmute(f) };
                f(target, pname, param)
            }
        )*
        fn tex_param_shim_for(name: &str) -> Option<usize> {
            match name {
                $(stringify!($name) => Some($name as *const () as usize),)*
                _ => None,
            }
        }
    };
}

tex_param_shims! {
    glTexParameteri(i32);
    glTexParameterx(i32);
    glTexParameterf(f32);
}

// Single-precision libm (soft-float guest -> hard-float VFPv2 host)
extern "aapcs" fn n72_sinf(x: f32) -> f32 { x.sin() }
extern "aapcs" fn n72_cosf(x: f32) -> f32 { x.cos() }
extern "aapcs" fn n72_tanf(x: f32) -> f32 { x.tan() }
extern "aapcs" fn n72_asinf(x: f32) -> f32 { x.asin() }
extern "aapcs" fn n72_acosf(x: f32) -> f32 { x.acos() }
extern "aapcs" fn n72_atanf(x: f32) -> f32 { x.atan() }
extern "aapcs" fn n72_atan2f(y: f32, x: f32) -> f32 { y.atan2(x) }
extern "aapcs" fn n72_sqrtf(x: f32) -> f32 { x.sqrt() }
extern "aapcs" fn n72_fabsf(x: f32) -> f32 { x.abs() }
extern "aapcs" fn n72_floorf(x: f32) -> f32 { x.floor() }
extern "aapcs" fn n72_ceilf(x: f32) -> f32 { x.ceil() }
extern "aapcs" fn n72_fmodf(x: f32, y: f32) -> f32 { x % y }
extern "aapcs" fn n72_powf(x: f32, y: f32) -> f32 { x.powf(y) }
extern "aapcs" fn n72_expf(x: f32) -> f32 { x.exp() }
extern "aapcs" fn n72_logf(x: f32) -> f32 { x.ln() }
extern "aapcs" fn n72_log10f(x: f32) -> f32 { x.log10() }

// Double-precision libm (1 or 2 doubles fit in r0:r1 and r2:r3 in AAPCS)
extern "aapcs" fn n72_sin(x: f64) -> f64 { x.sin() }
extern "aapcs" fn n72_cos(x: f64) -> f64 { x.cos() }
extern "aapcs" fn n72_tan(x: f64) -> f64 { x.tan() }
extern "aapcs" fn n72_asin(x: f64) -> f64 { x.asin() }
extern "aapcs" fn n72_acos(x: f64) -> f64 { x.acos() }
extern "aapcs" fn n72_atan(x: f64) -> f64 { x.atan() }
extern "aapcs" fn n72_atan2(y: f64, x: f64) -> f64 { y.atan2(x) }
extern "aapcs" fn n72_sqrt(x: f64) -> f64 { x.sqrt() }
extern "aapcs" fn n72_fabs(x: f64) -> f64 { x.abs() }
extern "aapcs" fn n72_floor(x: f64) -> f64 { x.floor() }
extern "aapcs" fn n72_ceil(x: f64) -> f64 { x.ceil() }
extern "aapcs" fn n72_fmod(x: f64, y: f64) -> f64 { x % y }
extern "aapcs" fn n72_pow(x: f64, y: f64) -> f64 { x.powf(y) }
extern "aapcs" fn n72_exp(x: f64) -> f64 { x.exp() }
extern "aapcs" fn n72_log(x: f64) -> f64 { x.ln() }
extern "aapcs" fn n72_log10(x: f64) -> f64 { x.log10() }
extern "aapcs" fn n72_abs(x: i32) -> i32 { x.wrapping_abs() }

// Direct libc ctype/string/memory calls (guest pointers == host pointers).
// Zero-length guards keep null pointers with n == 0 safe; memcpy routes to
// memmove to match touchHLE's GenericChar::memcpy overlap tolerance.
extern "aapcs" fn n72_tolower(c: i32) -> i32 {
    if (c as u8) as i32 == c {
        (c as u8).to_ascii_lowercase() as i32
    } else {
        c
    }
}
extern "aapcs" fn n72_toupper(c: i32) -> i32 {
    if (c as u8) as i32 == c {
        (c as u8).to_ascii_uppercase() as i32
    } else {
        c
    }
}
extern "aapcs" fn n72_memmove(dst: *mut libc::c_void, src: *const libc::c_void, n: usize) -> *mut libc::c_void {
    if n != 0 {
        unsafe { libc::memmove(dst, src, n) };
    }
    dst
}
extern "aapcs" fn n72_memset(dst: *mut libc::c_void, c: i32, n: usize) -> *mut libc::c_void {
    if n != 0 {
        unsafe { libc::memset(dst, c, n) };
    }
    dst
}
extern "aapcs" fn n72_bzero(dst: *mut libc::c_void, n: usize) {
    if n != 0 {
        unsafe { libc::memset(dst, 0, n) };
    }
}
extern "aapcs" fn n72_memcmp(a: *const libc::c_void, b: *const libc::c_void, n: usize) -> i32 {
    if n == 0 {
        return 0;
    }
    unsafe { libc::memcmp(a, b, n) }
}
extern "aapcs" fn n72_memchr(s: *const libc::c_void, c: i32, n: usize) -> *const libc::c_void {
    if n == 0 {
        return std::ptr::null();
    }
    unsafe { libc::memchr(s, c, n) }
}
extern "aapcs" fn n72_strlen(s: *const libc::c_char) -> usize {
    unsafe { libc::strlen(s) }
}
extern "aapcs" fn n72_strcmp(a: *const libc::c_char, b: *const libc::c_char) -> i32 {
    unsafe { libc::strcmp(a, b) }
}
extern "aapcs" fn n72_strncmp(a: *const libc::c_char, b: *const libc::c_char, n: usize) -> i32 {
    if n == 0 {
        return 0;
    }
    unsafe { libc::strncmp(a, b, n) }
}
extern "aapcs" fn n72_strcasecmp(a: *const libc::c_char, b: *const libc::c_char) -> i32 {
    unsafe { libc::strcasecmp(a, b) }
}
extern "aapcs" fn n72_strncasecmp(a: *const libc::c_char, b: *const libc::c_char, n: usize) -> i32 {
    if n == 0 {
        return 0;
    }
    unsafe { libc::strncasecmp(a, b, n) }
}
extern "aapcs" fn n72_strcpy(dst: *mut libc::c_char, src: *const libc::c_char) -> *mut libc::c_char {
    unsafe { libc::strcpy(dst, src) }
}
extern "aapcs" fn n72_strncpy(dst: *mut libc::c_char, src: *const libc::c_char, n: usize) -> *mut libc::c_char {
    if n != 0 {
        unsafe { libc::strncpy(dst, src, n) };
    }
    dst
}
extern "aapcs" fn n72_strchr(s: *const libc::c_char, c: i32) -> *const libc::c_char {
    unsafe { libc::strchr(s, c as u8 as libc::c_int) }
}
extern "aapcs" fn n72_strrchr(s: *const libc::c_char, c: i32) -> *const libc::c_char {
    unsafe { libc::strrchr(s, c as u8 as libc::c_int) }
}
extern "aapcs" fn n72_strstr(haystack: *const libc::c_char, needle: *const libc::c_char) -> *const libc::c_char {
    unsafe { libc::strstr(haystack, needle) }
}

// Direct wide-char string/memory calls (`wchar_t` = `i32` on Darwin ARM,
// matching `src/libc/wchar.rs`). Implemented directly rather than calling
// musl's `wcs*` so behavior matches `GenericChar<i32>` and null pointers
// with `n == 0` are safe.
extern "aapcs" fn n72_wcslen(s: *const i32) -> usize {
    let mut i = 0;
    unsafe {
        while s.add(i).read_unaligned() != 0 {
            i += 1;
        }
    }
    i
}
extern "aapcs" fn n72_wcscmp(a: *const i32, b: *const i32) -> i32 {
    let mut i = 0;
    unsafe {
        loop {
            let ca = a.add(i).read_unaligned();
            let cb = b.add(i).read_unaligned();
            if ca != cb {
                return if ca < cb { -1 } else { 1 };
            }
            if ca == 0 {
                return 0;
            }
            i += 1;
        }
    }
}
extern "aapcs" fn n72_wcsncmp(a: *const i32, b: *const i32, n: usize) -> i32 {
    let mut i = 0;
    unsafe {
        while i < n {
            let ca = a.add(i).read_unaligned();
            let cb = b.add(i).read_unaligned();
            if ca != cb {
                return if ca < cb { -1 } else { 1 };
            }
            if ca == 0 {
                return 0;
            }
            i += 1;
        }
    }
    0
}
extern "aapcs" fn n72_wcscpy(dst: *mut i32, src: *const i32) -> *mut i32 {
    let mut i = 0;
    unsafe {
        loop {
            let c = src.add(i).read_unaligned();
            dst.add(i).write_unaligned(c);
            if c == 0 {
                break;
            }
            i += 1;
        }
    }
    dst
}
extern "aapcs" fn n72_wcsncpy(dst: *mut i32, src: *const i32, n: usize) -> *mut i32 {
    let mut i = 0;
    let mut ended = false;
    unsafe {
        while i < n {
            if !ended {
                let c = src.add(i).read_unaligned();
                if c == 0 {
                    ended = true;
                }
                dst.add(i).write_unaligned(c);
            } else {
                dst.add(i).write_unaligned(0);
            }
            i += 1;
        }
    }
    dst
}
extern "aapcs" fn n72_wmemmove(dst: *mut i32, src: *const i32, n: usize) -> *mut i32 {
    if n != 0 {
        unsafe { libc::memmove(dst.cast(), src.cast(), n * std::mem::size_of::<i32>()) };
    }
    dst
}
extern "aapcs" fn n72_wmemset(dst: *mut i32, c: i32, n: usize) -> *mut i32 {
    let mut i = 0;
    unsafe {
        while i < n {
            dst.add(i).write_unaligned(c);
            i += 1;
        }
    }
    dst
}
extern "aapcs" fn n72_wmemcmp(a: *const i32, b: *const i32, n: usize) -> i32 {
    let mut i = 0;
    unsafe {
        while i < n {
            let ca = a.add(i).read_unaligned();
            let cb = b.add(i).read_unaligned();
            if ca != cb {
                return if ca < cb { -1 } else { 1 };
            }
            i += 1;
        }
    }
    0
}
extern "aapcs" fn n72_wmemchr(s: *const i32, c: i32, n: usize) -> *const i32 {
    let mut i = 0;
    unsafe {
        while i < n {
            if s.add(i).read_unaligned() == c {
                return s.add(i);
            }
            i += 1;
        }
    }
    std::ptr::null()
}

/// The host function (guest calling convention) for an import, and whether
/// it needs the GL context ready.
fn target(symbol: &str) -> Option<(usize, bool)> {
    let name = symbol.strip_prefix('_')?;
    match name {
        "sinf" => return Some((n72_sinf as *const () as usize, false)),
        "cosf" => return Some((n72_cosf as *const () as usize, false)),
        "tanf" => return Some((n72_tanf as *const () as usize, false)),
        "asinf" => return Some((n72_asinf as *const () as usize, false)),
        "acosf" => return Some((n72_acosf as *const () as usize, false)),
        "atanf" => return Some((n72_atanf as *const () as usize, false)),
        "atan2f" => return Some((n72_atan2f as *const () as usize, false)),
        "sqrtf" => return Some((n72_sqrtf as *const () as usize, false)),
        "fabsf" => return Some((n72_fabsf as *const () as usize, false)),
        "floorf" => return Some((n72_floorf as *const () as usize, false)),
        "ceilf" => return Some((n72_ceilf as *const () as usize, false)),
        "fmodf" => return Some((n72_fmodf as *const () as usize, false)),
        "powf" => return Some((n72_powf as *const () as usize, false)),
        "expf" => return Some((n72_expf as *const () as usize, false)),
        "logf" => return Some((n72_logf as *const () as usize, false)),
        "log10f" => return Some((n72_log10f as *const () as usize, false)),
        "sin" => return Some((n72_sin as *const () as usize, false)),
        "cos" => return Some((n72_cos as *const () as usize, false)),
        "tan" => return Some((n72_tan as *const () as usize, false)),
        "asin" => return Some((n72_asin as *const () as usize, false)),
        "acos" => return Some((n72_acos as *const () as usize, false)),
        "atan" => return Some((n72_atan as *const () as usize, false)),
        "atan2" => return Some((n72_atan2 as *const () as usize, false)),
        "sqrt" => return Some((n72_sqrt as *const () as usize, false)),
        "fabs" => return Some((n72_fabs as *const () as usize, false)),
        "floor" => return Some((n72_floor as *const () as usize, false)),
        "ceil" => return Some((n72_ceil as *const () as usize, false)),
        "fmod" => return Some((n72_fmod as *const () as usize, false)),
        "pow" => return Some((n72_pow as *const () as usize, false)),
        "exp" => return Some((n72_exp as *const () as usize, false)),
        "log" => return Some((n72_log as *const () as usize, false)),
        "log10" => return Some((n72_log10 as *const () as usize, false)),
        "abs" => return Some((n72_abs as *const () as usize, false)),
        "__tolower" => return Some((n72_tolower as *const () as usize, false)),
        "__toupper" => return Some((n72_toupper as *const () as usize, false)),
        "memcpy" | "memmove" => return Some((n72_memmove as *const () as usize, false)),
        "memset" => return Some((n72_memset as *const () as usize, false)),
        "bzero" => return Some((n72_bzero as *const () as usize, false)),
        "memcmp" => return Some((n72_memcmp as *const () as usize, false)),
        "memchr" => return Some((n72_memchr as *const () as usize, false)),
        "strlen" => return Some((n72_strlen as *const () as usize, false)),
        "strcmp" => return Some((n72_strcmp as *const () as usize, false)),
        "strncmp" => return Some((n72_strncmp as *const () as usize, false)),
        "strcasecmp" => return Some((n72_strcasecmp as *const () as usize, false)),
        "strncasecmp" => return Some((n72_strncasecmp as *const () as usize, false)),
        "strcpy" => return Some((n72_strcpy as *const () as usize, false)),
        "strncpy" => return Some((n72_strncpy as *const () as usize, false)),
        "strchr" => return Some((n72_strchr as *const () as usize, false)),
        "strrchr" => return Some((n72_strrchr as *const () as usize, false)),
        "strstr" => return Some((n72_strstr as *const () as usize, false)),
        "wcslen" => return Some((n72_wcslen as *const () as usize, false)),
        "wcscmp" => return Some((n72_wcscmp as *const () as usize, false)),
        "wcsncmp" => return Some((n72_wcsncmp as *const () as usize, false)),
        "wcscpy" => return Some((n72_wcscpy as *const () as usize, false)),
        "wcsncpy" => return Some((n72_wcsncpy as *const () as usize, false)),
        "wmemcpy" | "wmemmove" => return Some((n72_wmemmove as *const () as usize, false)),
        "wmemset" => return Some((n72_wmemset as *const () as usize, false)),
        "wmemcmp" => return Some((n72_wmemcmp as *const () as usize, false)),
        "wmemchr" => return Some((n72_wmemchr as *const () as usize, false)),
        _ => (),
    }
    if DIRECT_GL.contains(&name) || DIRECT_GL_UNSCALED.contains(&name) {
        let f = gl(name);
        return (f != 0).then_some((f, true));
    }
    if SHIM_GL.contains(&name) && gl(name) != 0 {
        return shim_for(name)
            .or_else(|| material_shim_for(name))
            .or_else(|| tex_param_shim_for(name))
            .map(|f| (f, true));
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
