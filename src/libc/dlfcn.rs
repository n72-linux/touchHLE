/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `dlfcn.h` (`dlopen()` and friends)

use crate::dyld::{export_c_func, FunctionExports};
use crate::mem::{ConstPtr, ConstVoidPtr, MutPtr, MutVoidPtr, Ptr, SafeRead};
use crate::Environment;
use std::collections::HashMap;

#[derive(Default)]
pub struct State {
    fname_cache: HashMap<u32, ConstPtr<u8>>,
}

const RTLD_DEFAULT: MutVoidPtr = Ptr::from_bits(-2 as _);

fn is_known_library(path: &str) -> bool {
    crate::dyld::DYLIB_LIST
        .iter()
        .any(|dylib| dylib.path == path || dylib.aliases.contains(&path))
}

fn dlopen(env: &mut Environment, path: ConstPtr<u8>, _mode: i32) -> MutVoidPtr {
    if path.is_null() {
        return RTLD_DEFAULT;
    }
    // TODO: dlopen() support for real dynamic libraries.
    assert!(is_known_library(env.mem.cstr_at_utf8(path).unwrap()));
    // For convenience, use the path as the handle.
    // TODO: Find out whether the handle is truly opaque on iPhone OS, and if
    // not, where it points.
    path.cast_mut().cast()
}

fn dlsym(env: &mut Environment, handle: MutVoidPtr, symbol: ConstPtr<u8>) -> MutVoidPtr {
    assert!(
        handle == RTLD_DEFAULT || is_known_library(env.mem.cstr_at_utf8(handle.cast()).unwrap())
    );
    // For some reason, the symbols passed to dlsym() don't have the leading _.
    let symbol = format!("_{}", env.mem.cstr_at_utf8(symbol).unwrap());
    // TODO: error handling. dlsym() should just return NULL in this case, but
    // currently it's probably more useful to have the emulator crash if there's
    // no symbol found, since it most likely indicates a missing host function.
    // TODO: Symbol lookup should be scoped to the specific library requested,
    // where appropriate!
    let addr = env
        .dyld
        .create_proc_address(&mut env.mem, &mut env.cpu, &symbol)
        .unwrap_or_else(|_| panic!("dlsym() for unimplemented function {symbol}"));
    Ptr::from_bits(addr.addr_with_thumb_bit())
}

fn dlclose(env: &mut Environment, handle: MutVoidPtr) -> i32 {
    assert!(
        handle == RTLD_DEFAULT || is_known_library(env.mem.cstr_at_utf8(handle.cast()).unwrap())
    );
    0 // success
}

#[repr(C, packed)]
struct DlInfo {
    dli_fname: ConstPtr<u8>,
    dli_fbase: ConstVoidPtr,
    dli_sname: ConstPtr<u8>,
    dli_saddr: ConstVoidPtr,
}
unsafe impl SafeRead for DlInfo {}

fn dladdr(env: &mut Environment, addr: ConstVoidPtr, info: MutPtr<DlInfo>) -> i32 {
    let addr_bits = addr.to_bits() & !1; // clear Thumb bit if set
    let matched = env.bins.iter().find_map(|bin| {
        let base = bin.text_segment_base;
        if addr_bits >= base && addr_bits < bin.last_segment_end {
            Some((base, bin.name.clone()))
        } else {
            None
        }
    });
    if let Some((base, name)) = matched {
        let dli_fname = if let Some(&cached) = env.libc_state.dlfcn.fname_cache.get(&base) {
            cached
        } else {
            let ptr = env.mem.alloc_and_write_cstr(name.as_bytes()).cast_const();
            env.libc_state.dlfcn.fname_cache.insert(base, ptr);
            ptr
        };
        let dli_fbase = Ptr::from_bits(base);
        env.mem.write(
            info,
            DlInfo {
                dli_fname,
                dli_fbase,
                dli_sname: Ptr::null(),
                dli_saddr: Ptr::null(),
            },
        );
        log_dbg!("dladdr({:?}) -> dli_fbase {:?} ({})", addr, dli_fbase, name);
        1
    } else {
        log_dbg!("dladdr({:?}) -> 0 (not found)", addr);
        0
    }
}

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(dlopen(_, _)),
    export_c_func!(dlsym(_, _)),
    export_c_func!(dlclose(_)),
    export_c_func!(dladdr(_, _)),
];
