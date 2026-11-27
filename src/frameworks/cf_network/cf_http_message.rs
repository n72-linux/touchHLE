/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `CFHTTPMessage`

use crate::abi::GuestFunction;
use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::frameworks::core_foundation::cf_allocator::{kCFAllocatorDefault, CFAllocatorRef};
use crate::frameworks::core_foundation::cf_data::CFDataRef;
use crate::frameworks::core_foundation::cf_string::CFStringRef;
use crate::frameworks::core_foundation::cf_url::CFURLRef;
use crate::frameworks::core_foundation::{CFIndex, CFOptionFlags, CFTypeRef};
use crate::frameworks::foundation::ns_string;
use crate::mem::{MutPtr, MutVoidPtr, Ptr};
use crate::objc::msg;
use crate::Environment;

// Note: on iOS SDK side this type is defined as a pointer to an opaque struct
type CFHTTPMessageRef = CFTypeRef;
type CFReadStreamRef = CFTypeRef;
type CFWriteStreamRef = CFTypeRef;
type CFHostRef = CFTypeRef;

fn CFHTTPMessageCreateRequest(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    request_method: CFStringRef,
    url: CFURLRef,
    http_version: CFStringRef,
) -> CFHTTPMessageRef {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default()); // unimplemented
    let url_desc = msg![env; url description];
    log!(
        "TODO: CFHTTPMessageCreateRequest({}, '{}', {}) -> NULL",
        ns_string::to_rust_string(env, request_method),
        ns_string::to_rust_string(env, url_desc),
        ns_string::to_rust_string(env, http_version),
    );
    Ptr::null()
}

fn CFHTTPMessageSetHeaderFieldValue(
    env: &mut Environment,
    message: CFHTTPMessageRef,
    header_field: CFStringRef,
    value: CFStringRef,
) {
    if !message.is_null() {
        todo!(
            "CFHTTPMessageSetHeaderFieldValue({:?}, {}, {})",
            message,
            ns_string::to_rust_string(env, header_field),
            ns_string::to_rust_string(env, value)
        );
    }
}

fn CFHTTPMessageSetBody(_env: &mut Environment, message: CFHTTPMessageRef, body_data: CFDataRef) {
    if !message.is_null() {
        todo!("CFHTTPMessageSetBody({:?}, {:?})", message, body_data);
    }
}

fn CFReadStreamCreateForHTTPRequest(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    message: CFHTTPMessageRef,
) -> CFReadStreamRef {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default()); // unimplemented
    if !message.is_null() {
        todo!(
            "CFReadStreamCreateForHTTPRequest({:?}, {:?})",
            allocator,
            message,
        );
    }
    Ptr::null()
}

fn CFReadStreamSetProperty(
    env: &mut Environment,
    stream: CFReadStreamRef,
    property_name: CFStringRef,
    property_value: CFTypeRef,
) -> bool {
    if !stream.is_null() {
        todo!(
            "CFReadStreamSetProperty({:?}, {}, {:?})",
            stream,
            ns_string::to_rust_string(env, property_name),
            property_value
        );
    }
    false
}
fn CFReadStreamSetClient(
    _env: &mut Environment,
    stream: CFReadStreamRef,
    stream_events: CFOptionFlags,
    client_callback: GuestFunction, // TODO: CFReadStreamClientCallBack
    client_context: MutVoidPtr,     // TODO: CFStreamClientContext *
) -> bool {
    if !stream.is_null() {
        todo!(
            "CFReadStreamSetClient({:?}, {}, {:?}, {:?})",
            stream,
            stream_events,
            client_callback,
            client_context
        );
    }
    false
}

fn CFHostCreateWithName(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    hostname: CFStringRef,
) -> CFHostRef {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default());
    log_once!("TODO: CFHostCreateWithName() -> NULL (ignoring host lookups)");
    log_dbg!(
        "TODO: CFHostCreateWithName('{}') -> NULL",
        ns_string::to_rust_string(env, hostname)
    );
    Ptr::null()
}

fn CFStreamCreatePairWithSocketToCFHost(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    host: CFHostRef,
    port: i32,
    read_stream: MutPtr<CFReadStreamRef>,
    write_stream: MutPtr<CFWriteStreamRef>,
) {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default());
    log!(
        "TODO: CFStreamCreatePairWithSocketToCFHost({:?}, {})",
        host,
        port
    );
    if !read_stream.is_null() {
        env.mem.write(read_stream, Ptr::null());
    }
    if !write_stream.is_null() {
        env.mem.write(write_stream, Ptr::null());
    }
}

fn CFReadStreamCreateForStreamedHTTPRequest(
    env: &mut Environment,
    allocator: CFAllocatorRef,
    request_headers: CFHTTPMessageRef,
    _request_body: CFReadStreamRef,
) -> CFReadStreamRef {
    assert!(allocator == kCFAllocatorDefault || env.mem.read(allocator).is_system_default());
    if !request_headers.is_null() {
        todo!(
            "CFReadStreamCreateForStreamedHTTPRequest({:?}, {:?})",
            allocator,
            request_headers,
        );
    }
    Ptr::null()
}

fn CFReadStreamOpen(_env: &mut Environment, stream: CFReadStreamRef) -> bool {
    assert!(stream.is_null());
    false
}

fn CFReadStreamClose(_env: &mut Environment, stream: CFReadStreamRef) {
    assert!(stream.is_null());
}

fn CFReadStreamScheduleWithRunLoop(
    _env: &mut Environment,
    stream: CFReadStreamRef,
    _run_loop: CFTypeRef,
    _run_loop_mode: CFStringRef,
) {
    assert!(stream.is_null());
}

fn CFReadStreamUnscheduleFromRunLoop(
    _env: &mut Environment,
    stream: CFReadStreamRef,
    _run_loop: CFTypeRef,
    _run_loop_mode: CFStringRef,
) {
    assert!(stream.is_null());
}

fn CFReadStreamRead(
    _env: &mut Environment,
    stream: CFReadStreamRef,
    _buffer: MutPtr<u8>,
    _buffer_length: CFIndex,
) -> CFIndex {
    assert!(stream.is_null());
    -1
}

fn CFReadStreamCopyProperty(
    _env: &mut Environment,
    stream: CFReadStreamRef,
    _property_name: CFStringRef,
) -> CFTypeRef {
    assert!(stream.is_null());
    Ptr::null()
}

fn CFReadStreamCopyError(_env: &mut Environment, stream: CFReadStreamRef) -> CFTypeRef {
    assert!(stream.is_null());
    Ptr::null()
}

fn CFWriteStreamOpen(_env: &mut Environment, stream: CFWriteStreamRef) -> bool {
    assert!(stream.is_null());
    false
}

fn CFWriteStreamClose(_env: &mut Environment, stream: CFWriteStreamRef) {
    assert!(stream.is_null());
}

fn CFWriteStreamGetStatus(_env: &mut Environment, stream: CFWriteStreamRef) -> CFIndex {
    assert!(stream.is_null());
    0
}

const kCFHTTPVersion1_0: &str = "kCFHTTPVersion1_0";
const kCFHTTPVersion1_1: &str = "kCFHTTPVersion1_1";

// TODO: move to correct location
const kCFStreamPropertySSLSettings: &str = "kCFStreamPropertySSLSettings";
const kCFStreamPropertyHTTPResponseHeader: &str = "kCFStreamPropertyHTTPResponseHeader";

pub const CONSTANTS: ConstantExports = &[
    (
        "_kCFHTTPVersion1_0",
        HostConstant::NSString(kCFHTTPVersion1_0),
    ),
    (
        "_kCFHTTPVersion1_1",
        HostConstant::NSString(kCFHTTPVersion1_1),
    ),
    (
        "_kCFStreamPropertySSLSettings",
        HostConstant::NSString(kCFStreamPropertySSLSettings),
    ),
    (
        "_kCFStreamPropertyHTTPResponseHeader",
        HostConstant::NSString(kCFStreamPropertyHTTPResponseHeader),
    ),
    // TODO: move to correct location
    (
        "_kCFStreamSocketSecurityLevelNegotiatedSSL",
        HostConstant::NSString("kCFStreamSocketSecurityLevelNegotiatedSSL"),
    ),
    (
        "_kCFStreamSSLValidatesCertificateChain",
        HostConstant::NSString("kCFStreamSSLValidatesCertificateChain"),
    ),
    (
        "_kCFStreamSSLPeerName",
        HostConstant::NSString("kCFStreamSSLPeerName"),
    ),
    (
        "_kCFStreamSSLLevel",
        HostConstant::NSString("kCFStreamSSLLevel"),
    ),
    (
        "_kCFStreamSSLAllowsExpiredRoots",
        HostConstant::NSString("kCFStreamSSLAllowsExpiredRoots"),
    ),
    (
        "_kCFStreamSSLAllowsExpiredCertificates",
        HostConstant::NSString("_kCFStreamSSLAllowsExpiredCertificates"),
    ),
    (
        "_kCFStreamSSLAllowsAnyRoot",
        HostConstant::NSString("kCFStreamSSLAllowsAnyRoot"),
    ),
];

pub const FUNCTIONS: FunctionExports = &[
    export_c_func!(CFHTTPMessageCreateRequest(_, _, _, _)),
    export_c_func!(CFHTTPMessageSetHeaderFieldValue(_, _, _)),
    export_c_func!(CFHTTPMessageSetBody(_, _)),
    export_c_func!(CFReadStreamCreateForHTTPRequest(_, _)),
    // TODO: move to correct location
    export_c_func!(CFReadStreamSetProperty(_, _, _)),
    export_c_func!(CFReadStreamSetClient(_, _, _, _)),
    export_c_func!(CFHostCreateWithName(_, _)),
    export_c_func!(CFStreamCreatePairWithSocketToCFHost(_, _, _, _, _)),
    export_c_func!(CFReadStreamCreateForStreamedHTTPRequest(_, _, _)),
    export_c_func!(CFReadStreamOpen(_)),
    export_c_func!(CFReadStreamClose(_)),
    export_c_func!(CFReadStreamScheduleWithRunLoop(_, _, _)),
    export_c_func!(CFReadStreamUnscheduleFromRunLoop(_, _, _)),
    export_c_func!(CFReadStreamRead(_, _, _)),
    export_c_func!(CFReadStreamCopyProperty(_, _)),
    export_c_func!(CFReadStreamCopyError(_)),
    export_c_func!(CFWriteStreamOpen(_)),
    export_c_func!(CFWriteStreamClose(_)),
    export_c_func!(CFWriteStreamGetStatus(_)),
];
