/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! `stdio.h`

use super::posix_io::{
    self, off_t, O_APPEND, O_CREAT, O_EXCL, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY, STDERR_FILENO,
    STDIN_FILENO, STDOUT_FILENO,
};
use crate::dyld::{export_c_func, ConstantExports, FunctionExports, HostConstant};
use crate::fs::GuestPath;
use crate::libc::errno::{get_errno, set_errno, EBUSY, EEXIST};
use crate::libc::string::strlen;
use crate::mem::{ConstPtr, ConstVoidPtr, GuestUSize, Mem, MutPtr, MutVoidPtr, Ptr, SafeRead};
use crate::Environment;

use crate::environment::{ThreadBlock, ThreadId};
use crate::libc::mach::time::mach_absolute_time;
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::io::Write;

// Standard C functions

pub mod printf;

const EOF: i32 = -1;
const READ_BUF_SIZE: usize = 8192;

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

struct FILEHostObject {
    /// `ungetc()` implementation
    pushbacks: Vec<u8>,
    lock_count: u32,
    owning_thread: Option<ThreadId>,
    buf_enabled: bool,
    read_buf: Vec<u8>,
    buf_pos: usize,
    buf_len: usize,
    cached_pos: Option<off_t>,
}

impl FILEHostObject {
    fn new(buf_enabled: bool) -> Self {
        Self {
            pushbacks: Vec::new(),
            lock_count: 0,
            owning_thread: None,
            buf_enabled,
            read_buf: Vec::new(),
            buf_pos: 0,
            buf_len: 0,
            cached_pos: if buf_enabled { Some(0) } else { None },
        }
    }
}

#[allow(clippy::upper_case_acronyms)]
/// C `FILE` struct. This is an opaque type in C, so the definition here is our
/// own.
pub(crate) struct FILE {
    fd: posix_io::FileDescriptor,
}
unsafe impl SafeRead for FILE {}

#[derive(Default)]
pub struct State {
    file_streams: HashMap<MutPtr<FILE>, FILEHostObject, FxBuildHasher>,
}
impl State {
    fn get_mut(env: &mut Environment) -> &mut Self {
        &mut env.libc_state.stdio
    }
    fn get_file_host_obj_mut(
        &mut self,
        mem: &mut Mem,
        file_ptr: MutPtr<FILE>,
    ) -> &mut FILEHostObject {
        let FILE { fd } = mem.read(file_ptr);
        match self.file_streams.entry(file_ptr) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                assert!(matches!(fd, STDIN_FILENO | STDOUT_FILENO | STDERR_FILENO));
                // Special case, need to do a lazy creation of host object
                entry.insert(FILEHostObject::new(false))
            }
        }
    }
    #[inline]
    fn get_checked_mut(
        &mut self,
        mem: &mut Mem,
        file_ptr: MutPtr<FILE>,
        current_thread: ThreadId,
    ) -> (posix_io::FileDescriptor, &mut FILEHostObject) {
        let FILE { fd } = mem.read(file_ptr);
        let obj = match self.file_streams.entry(file_ptr) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => {
                assert!(matches!(fd, STDIN_FILENO | STDOUT_FILENO | STDERR_FILENO));
                entry.insert(FILEHostObject::new(false))
            }
        };
        assert!(
            (obj.owning_thread.is_none() && obj.lock_count == 0)
                || obj.owning_thread == Some(current_thread)
        );
        (fd, obj)
    }
    pub(crate) fn try_acquire_file_object_lock(
        &mut self,
        mem: &mut Mem,
        file_ptr: MutPtr<FILE>,
        thread_id: ThreadId,
    ) -> bool {
        let FILEHostObject {
            lock_count,
            owning_thread,
            ..
        } = self.get_file_host_obj_mut(mem, file_ptr);
        if *lock_count == 0 {
            assert!(owning_thread.is_none());
            *lock_count = 1;
            *owning_thread = Some(thread_id);
            true
        } else {
            false
        }
    }
}

fn _touchHLE_check_file_object_lock(
    env: &mut Environment,
    file_ptr: MutPtr<FILE>,
) -> posix_io::FileDescriptor {
    let current_thread = env.current_thread;
    let (fd, _) = env
        .libc_state
        .stdio
        .get_checked_mut(&mut env.mem, file_ptr, current_thread);
    fd
}

/// Rewind any unread buffered bytes back to the OS fd and drop cached position.
fn sync_and_discard_read_buf(
    env: &mut Environment,
    file_ptr: MutPtr<FILE>,
    disable_buf: bool,
) -> posix_io::FileDescriptor {
    let current_thread = env.current_thread;
    let (fd, rem) = {
        let (fd, obj) = env
            .libc_state
            .stdio
            .get_checked_mut(&mut env.mem, file_ptr, current_thread);
        let rem = obj.buf_len - obj.buf_pos;
        obj.buf_pos = 0;
        obj.buf_len = 0;
        obj.cached_pos = None;
        if disable_buf {
            obj.buf_enabled = false;
        }
        (fd, rem)
    };
    if rem > 0 {
        let res = posix_io::lseek(env, fd, -(rem as off_t), SEEK_CUR);
        assert!(res >= 0);
    }
    fd
}

#[allow(non_camel_case_types)]
type fpos_t = off_t;

fn fopen(env: &mut Environment, filename: ConstPtr<u8>, mode: ConstPtr<u8>) -> MutPtr<FILE> {
    // Some testing on macOS suggests Apple's implementation will just ignore
    // flags it doesn't know about, and unfortunately real-world apps seem to
    // rely on this, e.g. using "wt" to mean open for writing in text mode,
    // even though that's not a real flag. The one thing that is required is for
    // a known basic mode (r/w/a) to come first.

    let mode = env.mem.cstr_at(mode);
    let [basic_mode @ (b'r' | b'w' | b'a'), flags @ ..] = mode else {
        panic!(
            "Unexpected or missing fopen() mode first character: {:?}",
            mode.first()
        );
    };
    let mut plus = false;
    for &flag in flags {
        match flag {
            // binary flag does nothing on UNIX
            b'b' => (),
            b'+' => plus = true,
            other => {
                log!("Tolerating unrecognized fopen() mode flag: {:?}", other);
            }
        }
    }

    let flags = match (basic_mode, plus) {
        (b'r', false) => O_RDONLY,
        (b'r', true) => O_RDWR,
        (b'w', false) => O_WRONLY | O_CREAT | O_TRUNC,
        (b'w', true) => O_RDWR | O_CREAT | O_TRUNC,
        (b'a', false) => O_WRONLY | O_APPEND | O_CREAT,
        (b'a', true) => O_RDWR | O_APPEND | O_CREAT,
        _ => unreachable!(),
    };

    match posix_io::open_direct(env, filename, flags) {
        -1 => Ptr::null(),
        fd => {
            let res = env.mem.alloc_and_write(FILE { fd });
            assert!(!State::get_mut(env).file_streams.contains_key(&res));
            State::get_mut(env)
                .file_streams
                .insert(res, FILEHostObject::new(flags == O_RDONLY));
            res
        }
    }
}

fn fread(
    env: &mut Environment,
    mut buffer: MutVoidPtr,
    item_size: GuestUSize,
    n_items: GuestUSize,
    file_ptr: MutPtr<FILE>,
) -> GuestUSize {
    // TODO: handle errno properly
    set_errno(env, 0);

    let current_thread = env.current_thread;

    if item_size == 0 {
        _ = env
            .libc_state
            .stdio
            .get_checked_mut(&mut env.mem, file_ptr, current_thread);
        return 0;
    }

    // Yes, the item_size/n_items split doesn't mean anything. The C standard
    // really does expect you to just multiply and divide like this, with no
    // attempt being made to ensure a whole number are read or written!
    let mut total_size = item_size.checked_mul(n_items).unwrap();
    let (fd, obj) = env
        .libc_state
        .stdio
        .get_checked_mut(&mut env.mem, file_ptr, current_thread);

    let mut already_read = if !obj.pushbacks.is_empty() {
        let to_copy = obj.pushbacks.len().min(total_size as usize);
        let offset = obj.pushbacks.len() - to_copy;

        _ = &obj.pushbacks[offset..].reverse();
        let to_copy_u32: GuestUSize = to_copy.try_into().unwrap();
        env.mem
            .bytes_at_mut(buffer.cast(), to_copy_u32)
            .copy_from_slice(&obj.pushbacks[offset..]);
        obj.pushbacks.truncate(offset);

        if total_size == to_copy_u32 {
            return total_size / item_size;
        }
        total_size -= to_copy_u32;
        let ptr: MutPtr<u8> = buffer.cast();
        buffer = (ptr + to_copy_u32).cast();
        to_copy_u32
    } else {
        0
    };

    if obj.buf_enabled {
        let avail = obj.buf_len - obj.buf_pos;
        if avail > 0 && total_size > 0 {
            let to_copy = avail.min(total_size as usize);
            env.mem
                .bytes_at_mut(buffer.cast(), to_copy as GuestUSize)
                .copy_from_slice(&obj.read_buf[obj.buf_pos..obj.buf_pos + to_copy]);
            obj.buf_pos += to_copy;
            if let Some(p) = &mut obj.cached_pos {
                *p += to_copy as off_t;
            }
            let to_copy_u32 = to_copy as GuestUSize;
            already_read += to_copy_u32;
            total_size -= to_copy_u32;
            if total_size == 0 {
                return already_read / item_size;
            }
            let ptr: MutPtr<u8> = buffer.cast();
            buffer = (ptr + to_copy_u32).cast();
        }
        if total_size == 0 {
            return already_read / item_size;
        }
        obj.buf_pos = 0;
        obj.buf_len = 0;
        if (total_size as usize) < READ_BUF_SIZE {
            let mut buf = std::mem::take(&mut obj.read_buf);
            if buf.len() < READ_BUF_SIZE {
                buf.resize(READ_BUF_SIZE, 0);
            }
            match posix_io::read_host(env, fd, &mut buf) {
                Err(()) => {
                    let obj = env.libc_state.stdio.file_streams.get_mut(&file_ptr).unwrap();
                    obj.read_buf = buf;
                    obj.cached_pos = None;
                    return already_read / item_size;
                }
                Ok(n) => {
                    let to_copy = n.min(total_size as usize);
                    if to_copy > 0 {
                        env.mem
                            .bytes_at_mut(buffer.cast(), to_copy as GuestUSize)
                            .copy_from_slice(&buf[..to_copy]);
                    }
                    let obj = env.libc_state.stdio.file_streams.get_mut(&file_ptr).unwrap();
                    obj.read_buf = buf;
                    obj.buf_pos = to_copy;
                    obj.buf_len = n;
                    if let Some(p) = &mut obj.cached_pos {
                        *p += to_copy as off_t;
                    }
                    return (already_read + to_copy as GuestUSize) / item_size;
                }
            }
        }
        match posix_io::read(env, fd, buffer, total_size) {
            // TODO: ferror() support.
            -1 => {
                if let Some(obj) = env.libc_state.stdio.file_streams.get_mut(&file_ptr) {
                    obj.cached_pos = None;
                }
                already_read / item_size
            }
            bytes_read => {
                let bytes_read: GuestUSize = bytes_read.try_into().unwrap();
                if let Some(obj) = env.libc_state.stdio.file_streams.get_mut(&file_ptr) {
                    if let Some(p) = &mut obj.cached_pos {
                        *p += bytes_read as off_t;
                    }
                }
                (bytes_read + already_read) / item_size
            }
        }
    } else {
        match posix_io::read(env, fd, buffer, total_size) {
            // TODO: ferror() support.
            -1 => already_read / item_size,
            bytes_read => {
                let bytes_read: GuestUSize = bytes_read.try_into().unwrap();
                (bytes_read + already_read) / item_size
            }
        }
    }
}

fn fgetc(env: &mut Environment, file_ptr: MutPtr<FILE>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    let current_thread = env.current_thread;
    let (fd, obj) = env
        .libc_state
        .stdio
        .get_checked_mut(&mut env.mem, file_ptr, current_thread);
    if let Some(pushback) = obj.pushbacks.pop() {
        obj.cached_pos = None;
        let new_offset = posix_io::lseek(env, fd, 1, SEEK_CUR);
        assert!(new_offset > 0); // TODO: handle error
        return pushback.into();
    }

    if obj.buf_enabled {
        if obj.buf_pos < obj.buf_len {
            let b = obj.read_buf[obj.buf_pos];
            obj.buf_pos += 1;
            if let Some(p) = &mut obj.cached_pos {
                *p += 1;
            }
            return b as i32;
        }
        obj.buf_pos = 0;
        obj.buf_len = 0;
        let mut buf = std::mem::take(&mut obj.read_buf);
        if buf.len() < READ_BUF_SIZE {
            buf.resize(READ_BUF_SIZE, 0);
        }
        return match posix_io::read_host(env, fd, &mut buf) {
            Err(()) => {
                let obj = env.libc_state.stdio.file_streams.get_mut(&file_ptr).unwrap();
                obj.read_buf = buf;
                obj.cached_pos = None;
                EOF
            }
            Ok(0) => {
                let obj = env.libc_state.stdio.file_streams.get_mut(&file_ptr).unwrap();
                obj.read_buf = buf;
                EOF
            }
            Ok(n) => {
                let b = buf[0];
                let obj = env.libc_state.stdio.file_streams.get_mut(&file_ptr).unwrap();
                obj.read_buf = buf;
                obj.buf_pos = 1;
                obj.buf_len = n;
                if let Some(p) = &mut obj.cached_pos {
                    *p += 1;
                }
                b as i32
            }
        };
    }

    let mut one = [0u8; 1];
    match posix_io::read_host(env, fd, &mut one) {
        Err(()) | Ok(0) => EOF,
        Ok(_) => one[0] as i32,
    }
}

fn getc(env: &mut Environment, file_ptr: MutPtr<FILE>) -> i32 {
    // `getc` is essentially identical to the `fgetc`
    fgetc(env, file_ptr)
}

/// `Handle getc() when the buffer ran out`
/// Ref <https://github.com/openbsd/src/blob/3b5bd91ebfd321ca2ed72ebf0b0debf1f4192118/lib/libc/stdio/rget.c#L43>
fn __srget(env: &mut Environment, file_ptr: MutPtr<FILE>) -> i32 {
    // OpenBSD implementation refills the internal buffer, but we don't
    getc(env, file_ptr)
}

fn ungetc(env: &mut Environment, c: i32, file_ptr: MutPtr<FILE>) -> i32 {
    assert!(c != EOF); // TODO
    let fd = sync_and_discard_read_buf(env, file_ptr, false);
    let curr_offset = posix_io::lseek(env, fd, 0, SEEK_CUR);
    assert!(curr_offset > 0);
    // Note: successful seeking clears EOF indicator
    let new_offset = posix_io::lseek(env, fd, -1, SEEK_CUR);
    assert!(new_offset >= 0); // TODO: handle error
    let FILEHostObject {
        ref mut pushbacks, ..
    } = env
        .libc_state
        .stdio
        .get_file_host_obj_mut(&mut env.mem, file_ptr);
    pushbacks.push(c.try_into().unwrap());
    log_dbg!("ungetc pushbacks: {:?}", pushbacks);
    c
}

fn fgets(
    env: &mut Environment,
    str: MutPtr<u8>,
    size: GuestUSize,
    stream: MutPtr<FILE>,
) -> MutPtr<u8> {
    let mut read = 0;
    let mut tmp = str;
    while read < size && fread(env, tmp.cast(), 1, 1, stream) != 0 {
        tmp += 1;
        read += 1;
        if env.mem.read(tmp - 1) == b'\n' {
            break;
        }
    }

    if read == 0 {
        return Ptr::null();
    } else {
        env.mem.write(tmp, b'\0');
    }
    str
}

fn fputs(env: &mut Environment, str: ConstPtr<u8>, stream: MutPtr<FILE>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    // TODO: this function doesn't set errno or return EOF yet
    let str_len = strlen(env, str);
    fwrite(env, str.cast(), str_len, 1, stream)
        .try_into()
        .unwrap()
}

fn fputc(env: &mut Environment, c: i32, stream: MutPtr<FILE>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    let ptr: MutPtr<u8> = env.mem.alloc_and_write(c.try_into().unwrap());
    let res = fwrite(env, ptr.cast_const().cast(), 1, 1, stream)
        .try_into()
        .unwrap();
    env.mem.free(ptr.cast());
    res
}

// From man page,
// `The putc() macro acts essentially identically to fputc(),
// but is a macro that expands in-line.`
fn putc(env: &mut Environment, c: i32, stream: MutPtr<FILE>) -> i32 {
    fputc(env, c, stream)
}

fn fwrite(
    env: &mut Environment,
    buffer: ConstVoidPtr,
    item_size: GuestUSize,
    n_items: GuestUSize,
    file_ptr: MutPtr<FILE>,
) -> GuestUSize {
    // TODO: handle errno properly
    set_errno(env, 0);

    if item_size == 0 || buffer.is_null() {
        return 0;
    }

    let fd = sync_and_discard_read_buf(env, file_ptr, false);

    let total_size = item_size.checked_mul(n_items).unwrap();

    // TODO: Refactor, use traits instead of this hack
    match fd {
        STDOUT_FILENO => {
            let buffer_slice = env.mem.bytes_at(buffer.cast(), total_size);
            match std::io::stdout().write(buffer_slice) {
                Ok(bytes_written) => (bytes_written / (item_size as usize)) as GuestUSize,
                Err(_err) => 0,
            }
        }
        STDERR_FILENO => {
            let buffer_slice = env.mem.bytes_at(buffer.cast(), total_size);
            match std::io::stderr().write(buffer_slice) {
                Ok(bytes_written) => (bytes_written / (item_size as usize)) as GuestUSize,
                Err(_err) => 0,
            }
        }
        _ => {
            // The comment about the item_size/n_items split in fread() applies
            // here too.
            match posix_io::write(env, fd, buffer, total_size) {
                // TODO: ferror() support.
                -1 => 0,
                bytes_written => {
                    let bytes_written: GuestUSize = bytes_written.try_into().unwrap();
                    bytes_written / item_size
                }
            }
        }
    }
}

const SEEK_SET: i32 = posix_io::SEEK_SET;
const SEEK_CUR: i32 = posix_io::SEEK_CUR;
const SEEK_END: i32 = posix_io::SEEK_END;
fn fseek(env: &mut Environment, file_ptr: MutPtr<FILE>, offset: i32, whence: i32) -> i32 {
    fseeko(env, file_ptr, offset.into(), whence)
}
fn fseeko(env: &mut Environment, file_ptr: MutPtr<FILE>, offset: off_t, whence: i32) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    assert!([SEEK_SET, SEEK_CUR, SEEK_END].contains(&whence));
    let current_thread = env.current_thread;
    let (fd, rem, buf_enabled) = {
        let (fd, obj) = env
            .libc_state
            .stdio
            .get_checked_mut(&mut env.mem, file_ptr, current_thread);
        if obj.buf_enabled && obj.pushbacks.is_empty() && (whence == SEEK_CUR || whence == SEEK_SET)
        {
            if let Some(cur_logical) = obj.cached_pos {
                let target = if whence == SEEK_CUR {
                    cur_logical.checked_add(offset)
                } else {
                    Some(offset)
                };
                if let Some(t) = target {
                    let buf_start = cur_logical - (obj.buf_pos as off_t);
                    let buf_end = buf_start + (obj.buf_len as off_t);
                    if t >= buf_start && t <= buf_end {
                        obj.buf_pos = (t - buf_start) as usize;
                        obj.cached_pos = Some(t);
                        posix_io::clearerr(env, fd);
                        return 0;
                    }
                }
            }
        }
        let rem = (obj.buf_len - obj.buf_pos) as off_t;
        obj.buf_pos = 0;
        obj.buf_len = 0;
        obj.cached_pos = None;
        (fd, rem, obj.buf_enabled)
    };

    let adj_offset = if whence == SEEK_CUR {
        offset.checked_sub(rem).unwrap()
    } else {
        offset
    };

    match posix_io::lseek(env, fd, adj_offset, whence) {
        -1 => -1,
        cur_pos => {
            let obj = env.libc_state.stdio.file_streams.get_mut(&file_ptr).unwrap();
            obj.pushbacks.clear();
            if buf_enabled {
                obj.cached_pos = Some(cur_pos);
            }
            0
        }
    }
}

fn ftell(env: &mut Environment, file_ptr: MutPtr<FILE>) -> i32 {
    // TODO: What's the correct behaviour if the position is beyond 2GiB?
    ftello(env, file_ptr).try_into().unwrap()
}
fn ftello(env: &mut Environment, file_ptr: MutPtr<FILE>) -> off_t {
    // TODO: handle errno properly
    set_errno(env, 0);

    let current_thread = env.current_thread;
    let (fd, rem) = {
        let (fd, obj) = env
            .libc_state
            .stdio
            .get_checked_mut(&mut env.mem, file_ptr, current_thread);
        if obj.pushbacks.is_empty() {
            if let Some(pos) = obj.cached_pos {
                return pos;
            }
        }
        (fd, (obj.buf_len - obj.buf_pos) as off_t)
    };
    let res = posix_io::lseek(env, fd, 0, posix_io::SEEK_CUR);
    if res == -1 {
        -1
    } else {
        res - rem
    }
}

fn rewind(env: &mut Environment, file_ptr: MutPtr<FILE>) {
    // TODO: handle errno properly
    set_errno(env, 0);

    // Note: this call will clean pushbacks as well
    fseek(env, file_ptr, 0, SEEK_SET);
}

fn fclose(env: &mut Environment, file_ptr: MutPtr<FILE>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    if file_ptr.is_null() {
        // According to the docs, this should segfault.
        // But as tested on iPhone Simulator, it doesn't
        log!("fclose(NULL) => EOF");
        return EOF;
    }

    let fd = _touchHLE_check_file_object_lock(env, file_ptr);
    if matches!(fd, STDIN_FILENO | STDOUT_FILENO | STDERR_FILENO) {
        log!(
            "Warning! fclose({:?}) is called for standard descriptor {}.",
            file_ptr,
            fd
        );
    }
    assert!(State::get_mut(env).file_streams.remove(&file_ptr).is_some());

    env.mem.free(file_ptr.cast());

    match posix_io::close(env, fd) {
        0 => 0,
        -1 => EOF,
        _ => unreachable!(),
    }
}

fn ferror(env: &mut Environment, file_ptr: MutPtr<FILE>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    _touchHLE_check_file_object_lock(env, file_ptr);

    log!("TODO: ferror() support.");
    0
}

fn fsetpos(env: &mut Environment, file_ptr: MutPtr<FILE>, pos: ConstPtr<fpos_t>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    let current_thread = env.current_thread;
    let (fd, buf_enabled) = {
        let (fd, obj) = env
            .libc_state
            .stdio
            .get_checked_mut(&mut env.mem, file_ptr, current_thread);
        obj.buf_pos = 0;
        obj.buf_len = 0;
        obj.cached_pos = None;
        (fd, obj.buf_enabled)
    };

    let res = posix_io::lseek(env, fd, env.mem.read(pos), SEEK_SET);
    if res == -1 {
        -1
    } else {
        let obj = env.libc_state.stdio.file_streams.get_mut(&file_ptr).unwrap();
        obj.pushbacks.clear();
        if buf_enabled {
            obj.cached_pos = Some(res);
        }
        0
    }
}

fn fgetpos(env: &mut Environment, file_ptr: MutPtr<FILE>, pos: MutPtr<fpos_t>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    let res = ftello(env, file_ptr);
    if res == -1 {
        return -1;
    }
    env.mem.write(pos, res);
    0
}

fn feof(env: &mut Environment, file_ptr: MutPtr<FILE>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    let current_thread = env.current_thread;
    let (fd, has_buffered) = {
        let (fd, obj) = env
            .libc_state
            .stdio
            .get_checked_mut(&mut env.mem, file_ptr, current_thread);
        (fd, !obj.pushbacks.is_empty() || obj.buf_pos < obj.buf_len)
    };
    if has_buffered {
        0
    } else {
        posix_io::eof(env, fd)
    }
}

fn clearerr(env: &mut Environment, file_ptr: MutPtr<FILE>) {
    // TODO: handle errno properly
    set_errno(env, 0);

    let fd = _touchHLE_check_file_object_lock(env, file_ptr);
    posix_io::clearerr(env, fd)
}

fn fflush(env: &mut Environment, file_ptr: MutPtr<FILE>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    let fd = sync_and_discard_read_buf(env, file_ptr, false);
    posix_io::fflush(env, fd)
}

fn puts(env: &mut Environment, s: ConstPtr<u8>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    let _ = std::io::stdout().write_all(env.mem.cstr_at(s));
    let _ = std::io::stdout().write_all(b"\n");
    // TODO: I/O error handling
    // TODO: is this the return value iPhone OS uses?
    0
}

fn putchar(env: &mut Environment, c: u8) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    let _ = std::io::stdout().write(std::slice::from_ref(&c));
    0
}

fn remove(env: &mut Environment, path: ConstPtr<u8>) -> i32 {
    // TODO: handle errno properly
    set_errno(env, 0);

    if Ptr::is_null(path) {
        // TODO: set errno
        log!("remove({:?}) => -1, attempted to remove null", path);
        return -1;
    }

    match env
        .fs
        .remove(GuestPath::new(&env.mem.cstr_at_utf8(path).unwrap()))
    {
        Ok(()) => {
            log_dbg!("remove({:?}) => 0", path);
            0
        }
        Err(_) => {
            // TODO: set errno
            log!("Warning: remove({:?}) failed, returning -1", path);
            -1
        }
    }
}

fn setbuf(env: &mut Environment, file_ptr: MutPtr<FILE>, buf: ConstPtr<u8>) {
    // TODO: handle errno properly
    set_errno(env, 0);

    sync_and_discard_read_buf(env, file_ptr, true);

    assert!(buf.is_null());
    log!(
        "Warning: ignoring a setbuf() for {:?} with NULL (unbuffered)",
        file_ptr
    );
}

fn tmpfile(env: &mut Environment) -> MutPtr<FILE> {
    // TODO: use `/tmp` instead of app specific tmp location
    let tmp_dir = env.fs.home_directory().join("tmp");
    let time = mach_absolute_time(env);
    let file = tmp_dir.join(time.to_string());
    log_dbg!("tmpfile {:?}", file);
    let tmp_filename = env.mem.alloc_and_write_cstr(file.as_str().as_bytes());
    let res = match posix_io::open_direct(env, tmp_filename.cast_const(), O_RDWR | O_CREAT | O_EXCL)
    {
        -1 => {
            assert!(get_errno(env) != EEXIST); // TODO
            Ptr::null()
        }
        fd => {
            let res = env.mem.alloc_and_write(FILE { fd });
            assert!(!State::get_mut(env).file_streams.contains_key(&res));
            State::get_mut(env)
                .file_streams
                .insert(res, FILEHostObject::new(false));
            res
        }
    };
    assert!(!res.is_null());
    // Unlinking the file immediately.
    env.fs.remove(&file).unwrap();
    env.mem.free(tmp_filename.cast());
    res
}

// POSIX-specific functions

fn fileno(env: &mut Environment, file_ptr: MutPtr<FILE>) -> posix_io::FileDescriptor {
    sync_and_discard_read_buf(env, file_ptr, true)
}

fn flockfile(env: &mut Environment, file_ptr: MutPtr<FILE>) {
    let FILEHostObject {
        ref mut lock_count,
        ref mut owning_thread,
        ..
    } = env
        .libc_state
        .stdio
        .get_file_host_obj_mut(&mut env.mem, file_ptr);
    match owning_thread {
        Some(thread_id) if *thread_id != env.current_thread => {
            env.yield_thread(ThreadBlock::FileObjectLock(file_ptr));
        }
        _ => {
            *lock_count = lock_count.checked_add(1).unwrap();
            *owning_thread = Some(env.current_thread);
        }
    }
}

fn ftrylockfile(env: &mut Environment, file_ptr: MutPtr<FILE>) -> i32 {
    let FILEHostObject {
        ref mut lock_count,
        ref mut owning_thread,
        ..
    } = env
        .libc_state
        .stdio
        .get_file_host_obj_mut(&mut env.mem, file_ptr);
    match owning_thread {
        Some(thread_id) if *thread_id != env.current_thread => EBUSY,
        _ => {
            *lock_count = lock_count.checked_add(1).unwrap();
            *owning_thread = Some(env.current_thread);
            0
        }
    }
}

fn funlockfile(env: &mut Environment, file_ptr: MutPtr<FILE>) {
    let FILEHostObject {
        ref mut lock_count,
        ref mut owning_thread,
        ..
    } = env
        .libc_state
        .stdio
        .get_file_host_obj_mut(&mut env.mem, file_ptr);
    assert_eq!(*owning_thread, Some(env.current_thread));
    *lock_count = lock_count.checked_sub(1).unwrap();
    if *lock_count == 0 {
        *owning_thread = None;
    }
}

pub const CONSTANTS: ConstantExports = &[
    (
        "___stdinp",
        HostConstant::Custom(|env| -> ConstVoidPtr {
            let ptr = env.mem.alloc_and_write(FILE { fd: STDIN_FILENO });
            // Note: Host object would be created lazily
            env.mem.alloc_and_write(ptr).cast().cast_const()
        }),
    ),
    (
        "___stdoutp",
        HostConstant::Custom(|env| -> ConstVoidPtr {
            let ptr = env.mem.alloc_and_write(FILE { fd: STDOUT_FILENO });
            // Note: Host object would be created lazily
            env.mem.alloc_and_write(ptr).cast().cast_const()
        }),
    ),
    (
        "___stderrp",
        HostConstant::Custom(|env| -> ConstVoidPtr {
            let ptr = env.mem.alloc_and_write(FILE { fd: STDERR_FILENO });
            // Note: Host object would be created lazily
            env.mem.alloc_and_write(ptr).cast().cast_const()
        }),
    ),
];

pub const FUNCTIONS: FunctionExports = &[
    // Standard C functions
    export_c_func!(fopen(_, _)),
    export_c_func!(fread(_, _, _, _)),
    export_c_func!(fgetc(_)),
    export_c_func!(getc(_)),
    export_c_func!(__srget(_)),
    export_c_func!(ungetc(_, _)),
    export_c_func!(fgets(_, _, _)),
    export_c_func!(fputs(_, _)),
    export_c_func!(fputc(_, _)),
    export_c_func!(putc(_, _)),
    export_c_func!(fwrite(_, _, _, _)),
    export_c_func!(fseek(_, _, _)),
    export_c_func!(fseeko(_, _, _)),
    export_c_func!(ftell(_)),
    export_c_func!(ftello(_)),
    export_c_func!(rewind(_)),
    export_c_func!(fsetpos(_, _)),
    export_c_func!(fgetpos(_, _)),
    export_c_func!(feof(_)),
    export_c_func!(clearerr(_)),
    export_c_func!(fflush(_)),
    export_c_func!(fclose(_)),
    export_c_func!(ferror(_)),
    export_c_func!(puts(_)),
    export_c_func!(putchar(_)),
    export_c_func!(remove(_)),
    export_c_func!(setbuf(_, _)),
    export_c_func!(tmpfile()),
    // POSIX-specific functions
    export_c_func!(fileno(_)),
    export_c_func!(flockfile(_)),
    export_c_func!(ftrylockfile(_)),
    export_c_func!(funlockfile(_)),
];
