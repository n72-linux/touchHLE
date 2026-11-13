/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Cross-platform memory management wrappers using the host's system calls.

/// Cross-platform memory allocation using the host's system calls.
/// Returns an address aligned to the guest's 4KB page boundaries.
///
/// - The function returns a raw pointer to allocated memory.
///   The caller is responsible for managing that memory.
/// - The returned pointer must be freed using the corresponding
///   [`free_memory`] call.
#[cfg(windows)]
pub(super) unsafe fn allocate_memory(size: usize) -> std::io::Result<*mut core::ffi::c_void> {
    use windows_sys::Win32::System::Memory::{
        VirtualAlloc, MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE,
    };

    let ptr = unsafe {
        VirtualAlloc(
            std::ptr::null(),
            size,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_READWRITE,
        )
    };

    if ptr.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    Ok(ptr)
}

#[cfg(all(unix, feature = "cpu-dynarmic"))]
pub(super) unsafe fn allocate_memory(size: usize) -> std::io::Result<*mut core::ffi::c_void> {
    use libc::{mmap, sysconf, MAP_ANONYMOUS, MAP_PRIVATE, PROT_READ, PROT_WRITE};

    const PAGE_SIZE: usize = crate::mem::PAGE_SIZE as usize;
    let host_page_size = unsafe { sysconf(libc::_SC_PAGESIZE) as usize };

    assert!(
        host_page_size >= PAGE_SIZE,
        "Hosts with smaller than 4KiB pages are not supported."
    );

    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            size,
            PROT_READ | PROT_WRITE,
            MAP_PRIVATE | MAP_ANONYMOUS,
            -1,
            0,
        )
    };

    if ptr == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    Ok(ptr)
}

/// Cross-platform memory free using the host's system calls.
///
/// # Safety
/// - The address and size should match parameters and result of the
///   [`allocate_memory`] call.
#[cfg(windows)]
pub(super) unsafe fn free_memory(
    address: *mut core::ffi::c_void,
    _size: usize,
) -> std::io::Result<()> {
    use windows_sys::Win32::System::Memory::{VirtualFree, MEM_RELEASE};

    let res = unsafe { VirtualFree(address, 0, MEM_RELEASE) };

    if res == 0 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}

#[cfg(unix)]
pub(super) unsafe fn free_memory(
    address: *mut core::ffi::c_void,
    size: usize,
) -> std::io::Result<()> {
    use libc::munmap;

    let res = unsafe { munmap(address, size) };

    if res == -1 {
        return Err(std::io::Error::last_os_error());
    }

    Ok(())
}

/// n72 fork, native CPU backend: reserve the guest arena at its fixed place
/// ([crate::cpu::ARENA_BASE]..[crate::cpu::ARENA_END], readable, writable
/// and executable, with no swap reservation) and return its base.
///
/// 0x1000 is only mappable with the N72 kernel's CONFIG_ARM_LOW_USER_MAPPINGS;
/// without it ARM Linux refuses anything below 0x2000, and then apps whose
/// code starts below 0x2000 cannot run.
#[cfg(feature = "cpu-native")]
pub(super) unsafe fn reserve_guest_arena() -> crate::mem::VAddr {
    use crate::cpu::{ARENA_BASE, ARENA_BASE_FALLBACK, ARENA_END};
    use libc::{
        mmap, MAP_ANONYMOUS, MAP_FAILED, MAP_FIXED_NOREPLACE, MAP_NORESERVE, MAP_PRIVATE,
        PROT_EXEC, PROT_READ, PROT_WRITE,
    };

    for base in [ARENA_BASE, ARENA_BASE_FALLBACK] {
        let ptr = unsafe {
            mmap(
                base as usize as *mut core::ffi::c_void,
                (ARENA_END - base) as usize,
                PROT_READ | PROT_WRITE | PROT_EXEC,
                MAP_PRIVATE | MAP_ANONYMOUS | MAP_NORESERVE | MAP_FIXED_NOREPLACE,
                -1,
                0,
            )
        };
        if ptr != MAP_FAILED && ptr as usize == base as usize {
            if base != ARENA_BASE {
                log!(
                    "Warning: guest memory starts at {:#x}, not {:#x} (kernel without \
                     CONFIG_ARM_LOW_USER_MAPPINGS). Apps with code below {:#x} will crash.",
                    base,
                    ARENA_BASE,
                    base
                );
            }
            return base;
        }
        if ptr != MAP_FAILED {
            // An old kernel that ignores MAP_FIXED_NOREPLACE put it elsewhere.
            unsafe { libc::munmap(ptr, (ARENA_END - base) as usize) };
        }
    }
    panic!(
        "Could not reserve guest memory at {:#x}..{:#x}: {} (is the host linked above it?)",
        ARENA_BASE,
        ARENA_END,
        std::io::Error::last_os_error()
    );
}

/// n72 fork, native CPU backend: make guest pages inaccessible (for the part
/// of __PAGEZERO that lies inside the arena).
#[cfg(feature = "cpu-native")]
pub(super) unsafe fn protect_none(base: crate::mem::VAddr, size: crate::mem::GuestUSize) {
    let res = unsafe { libc::mprotect(base as usize as *mut _, size as usize, libc::PROT_NONE) };
    assert!(
        res == 0,
        "mprotect failed: {}",
        std::io::Error::last_os_error()
    );
}
