/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CFArray` and `CFMutableArray`.
//!
//! These are toll-free bridged to `NSArray` and `NSMutableArray` in Apple's
//! implementation. Here they are the same types.

use super::cf_allocator::{kCFAllocatorDefault, CFAllocatorRef};
use super::cf_string::CFStringRef;
use super::cf_type::CFEqual;
use super::{CFIndex, CFRelease, CFRetain};
use crate::abi::{CallFromHost, GuestFunction};
use crate::dyld::{
    export_c_func, ConstantExports, Dyld, FunctionExports, HostConstant, HostFunction,
};
use crate::frameworks::foundation::{ns_array, NSUInteger};
use crate::mem::{ConstPtr, ConstVoidPtr, Mem, SafeRead};
use crate::objc::{id, msg, msg_class};
use crate::Environment;

#[allow(dead_code)]
pub type CFArrayRef = super::CFTypeRef;
pub type CFMutableArrayRef = super::CFTypeRef;

#[derive(Copy, Clone, Debug)]
#[repr(C, packed)]
pub struct CFArrayCallBacks {
    pub version: CFIndex,
    pub retain: GuestFunction,
    pub release: GuestFunction,
    pub copy_desc: GuestFunction,
    pub equal: GuestFunction,
}
unsafe impl SafeRead for CFArrayCallBacks {}

fn CFArrayCreate(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    values: ConstPtr<ConstVoidPtr>,
    num_values: CFIndex,
    callbacks: ConstPtr<CFArrayCallBacks>,
) -> CFArrayRef {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default()); // unimplemented
    assert!(num_values >= 0);

    let mut raw_values = Vec::with_capacity(num_values as usize);
    for i in 0..(num_values as u32) {
        raw_values.push(env.mem.read(values + i));
    }

    if callbacks.is_null() {
        let array: id = msg_class![env; _touchHLE_NSMutableArray_non_retaining new];
        for val in raw_values {
            let val: id = val.cast().cast_mut();
            msg![env; array addObject:val]
        }
        return array;
    }

    let cb = env.mem.read(callbacks);
    let retain_fn = cb.retain;
    if retain_fn.to_ptr().is_null() {
        let release_fn = cb.release;
        assert!(release_fn.to_ptr().is_null());
        let array: id = msg_class![env; _touchHLE_NSMutableArray_non_retaining new];
        for val in raw_values {
            let val: id = val.cast().cast_mut();
            msg![env; array addObject:val]
        }
        return array;
    }

    let mut objects = Vec::with_capacity(raw_values.len());
    for val in raw_values {
        let retained: ConstVoidPtr = retain_fn.call_from_host(env, (allocator, val));
        objects.push(retained.cast().cast_mut());
    }
    ns_array::from_vec(env, objects)
}

fn CFArrayCreateMutable(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    capacity: CFIndex,
    callbacks: ConstVoidPtr, // TODO, should be `const CFArrayCallBacks*`
) -> CFMutableArrayRef {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default()); // unimplemented
    assert!(capacity == 0); // TODO: fixed capacity support
    assert!(callbacks.is_null()); // TODO: support retaining etc

    msg_class![env; _touchHLE_NSMutableArray_non_retaining new]
}

fn CFArrayGetCount(env: &mut Environment, array: CFArrayRef) -> CFIndex {
    let count: NSUInteger = msg![env; array count];
    count.try_into().unwrap()
}

fn CFArrayGetValueAtIndex(env: &mut Environment, array: CFArrayRef, idx: CFIndex) -> ConstVoidPtr {
    let idx: NSUInteger = idx.try_into().unwrap();
    let value: id = msg![env; array objectAtIndex:idx];
    value.cast().cast_const()
}

fn CFArrayAppendValue(env: &mut Environment, array: CFMutableArrayRef, value: ConstVoidPtr) {
    let value: id = value.cast().cast_mut();
    msg![env; array addObject:value]
}

fn CFArrayRemoveValueAtIndex(env: &mut Environment, array: CFMutableArrayRef, idx: CFIndex) {
    let idx: NSUInteger = idx.try_into().unwrap();
    msg![env; array removeObjectAtIndex:idx]
}

fn CFArrayRemoveAllValues(env: &mut Environment, array: CFMutableArrayRef) {
    () = msg![env; array removeAllObjects];
}

fn _touchHLE_CFArray_retain(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    value: ConstVoidPtr,
) -> ConstVoidPtr {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default());
    CFRetain(env, value.cast_mut().cast()).cast_const().cast()
}
fn _touchHLE_CFArray_release(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    value: ConstVoidPtr,
) {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default());
    CFRelease(env, value.cast_mut().cast());
}
fn _touchHLE_CFArray_copyDescription(
    _env: &mut Environment,
    _value: ConstVoidPtr,
) -> CFStringRef {
    todo!()
}
fn _touchHLE_CFArray_equal(
    env: &mut Environment,
    value1: ConstVoidPtr,
    value2: ConstVoidPtr,
) -> bool {
    CFEqual(env, value1.cast_mut().cast(), value2.cast_mut().cast())
}

fn create_default_callback_functions(mem: &mut Mem, dyld: &mut Dyld) -> CFArrayCallBacks {
    let retain_hf: HostFunction =
        &(_touchHLE_CFArray_retain as fn(&mut Environment, _, _) -> _);
    let retain = dyld.create_guest_function(mem, "__touchHLE_CFArray_retain", retain_hf);

    let release_hf: HostFunction = &(_touchHLE_CFArray_release as fn(&mut Environment, _, _));
    let release = dyld.create_guest_function(mem, "__touchHLE_CFArray_release", release_hf);

    let copy_desc_hf: HostFunction =
        &(_touchHLE_CFArray_copyDescription as fn(&mut Environment, _) -> _);
    let copy_desc =
        dyld.create_guest_function(mem, "__touchHLE_CFArray_copyDescription", copy_desc_hf);

    let equal_hf: HostFunction = &(_touchHLE_CFArray_equal as fn(&mut Environment, _, _) -> _);
    let equal = dyld.create_guest_function(mem, "__touchHLE_CFArray_equal", equal_hf);

    CFArrayCallBacks {
        version: 0,
        retain,
        release,
        copy_desc,
        equal,
    }
}

pub const CONSTANTS: ConstantExports = &[(
    "_kCFTypeArrayCallBacks",
    HostConstant::Custom(|env| {
        let callbacks = create_default_callback_functions(&mut env.mem, &mut env.dyld);
        env.mem.alloc_and_write(callbacks).cast_void().cast_const()
    }),
)];

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CFArrayCreate(_, _, _, _)),
    export_c_func!(CFArrayCreateMutable(_, _, _)),
    export_c_func!(CFArrayGetCount(_)),
    export_c_func!(CFArrayGetValueAtIndex(_, _)),
    export_c_func!(CFArrayAppendValue(_, _)),
    export_c_func!(CFArrayRemoveValueAtIndex(_, _)),
    export_c_func!(CFArrayRemoveAllValues(_)),
];
