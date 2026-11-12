/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! CPU backends.
//!
//! - `cpu-dynarmic` (upstream): guest code is recompiled by dynarmic.
//! - `cpu-native` (n72 fork): guest code runs directly on a 32-bit ARMv6
//!   Linux host, with guest addresses equal to host addresses. See
//!   [native] and `N72-NATIVE-PLAN.md`.
//!
//! Both expose the same API: [Cpu], [CpuContext], [CpuState], [CpuError],
//! plus [write_svc] and [alloc_code], which the dynamic linker uses to place
//! calls into the host.

#[cfg(all(feature = "cpu-dynarmic", feature = "cpu-native"))]
compile_error!("enable exactly one of the features cpu-dynarmic and cpu-native");
#[cfg(not(any(feature = "cpu-dynarmic", feature = "cpu-native")))]
compile_error!("enable one of the features cpu-dynarmic or cpu-native");

#[cfg(feature = "cpu-dynarmic")]
mod dynarmic;
#[cfg(feature = "cpu-dynarmic")]
pub use dynarmic::*;

#[cfg(feature = "cpu-native")]
pub mod native;
#[cfg(feature = "cpu-native")]
pub use native::*;
#[cfg(feature = "cpu-native")]
pub mod native_selftest;

#[cfg(feature = "cpu-dynarmic")]
mod svc_dynarmic {
    use crate::mem::{GuestUSize, Mem, MutPtr, Ptr, VAddr};

    /// Write an instruction at `site` that calls into the host with SVC
    /// number `svc` and then continues at `site + 4`.
    pub fn write_svc(mem: &mut Mem, site: VAddr, svc: u32) {
        assert!(svc & 0xff000000 == 0);
        let ptr: MutPtr<u32> = Ptr::from_bits(site);
        mem.write(ptr, svc | 0xef000000);
    }

    /// Allocate guest memory for a small routine that contains SVC sites.
    pub fn alloc_code(mem: &mut Mem, size: GuestUSize) -> VAddr {
        mem.alloc(size).to_bits()
    }
}
#[cfg(feature = "cpu-dynarmic")]
pub use svc_dynarmic::{alloc_code, write_svc};
