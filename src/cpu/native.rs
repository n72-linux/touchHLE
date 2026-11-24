/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Native CPU backend (n72 fork): guest code runs directly on the host CPU.
//!
//! The host is 32-bit ARMv6 Linux (the iPod touch 2G's ARM1176). Guest
//! addresses are host addresses: [crate::mem::Mem] reserves the guest arena
//! at [ARENA_BASE]..[ARENA_END] and the host executable is linked above it.
//!
//! Control flow between host and guest:
//!
//! - **Host → guest**: [Cpu::run_or_step] calls `n72_enter_guest`, which
//!   saves the host's callee-saved state, loads every guest register and
//!   jumps to the guest PC (Thumb or ARM).
//! - **Guest → host**: the dynamic linker never writes `svc` (that would trap
//!   into Linux). [write_svc] writes `b entry` at the call site instead.
//!   Each site has its own 20-byte gate entry in a gate region within
//!   ±32 MiB of it:
//!
//!   ```text
//!   entry+0   add ip, pc, #4      @ ip = entry+12, the record
//!   entry+4   ldr pc, [pc, #-4]
//!   entry+8   .word n72_common_gate
//!   entry+12  .word svc number    @ record
//!   entry+16  .word site + 4      @ resume PC, as after a real SVC
//!   ```
//!
//!   `n72_common_gate` stores the guest registers (ip is scratch at a call
//!   boundary and is clobbered) and returns to the host, so `run_or_step`
//!   reports `CpuState::Svc(n)` with PC = site + 4, as dynarmic does.
//! - **Faults and time slices**: SIGSEGV/SIGBUS/SIGILL/SIGTRAP and a periodic
//!   timer signal are handled on an alternate stack. If the guest was
//!   running, the handler copies the guest state out of the `ucontext` and
//!   resumes at `n72_exit_to_host` with an error or "slice over" code.
//!
//! Enter and exit strictly alternate (the guest never runs while another
//! guest entry is pending), so one saved host stack pointer is enough, even
//! with host functions that call back into the guest and with touchHLE's
//! coroutine stacks.

use crate::abi::GuestFunction;
use crate::mem::{GuestUSize, Mem, MutPtr, Ptr, VAddr};
use std::collections::HashMap;

/// Lowest guest address. Mapping 0x1000 needs the N72 kernel's
/// CONFIG_ARM_LOW_USER_MAPPINGS (ARM Linux refuses anything below 0x2000
/// otherwise); [crate::mem::Mem::new] falls back to [ARENA_BASE_FALLBACK].
pub const ARENA_BASE: VAddr = 0x1000;
pub const ARENA_BASE_FALLBACK: VAddr = 0x2000;
/// End of the guest arena. The host executable is linked at 0x70000000 and
/// its shared libraries and mmaps go top-down from below the stack.
pub const ARENA_END: VAddr = 0x6000_0000;

/// Saved guest state. The layout is used by the assembly below: regs at 0,
/// cpsr at 64, fpscr at 68, d0-d15 at 72.
#[repr(C)]
#[derive(Clone, Debug)]
pub struct CpuContext {
    pub regs: [u32; 16],
    pub cpsr: u32,
    pub fpscr: u32,
    pub vfp: [u64; 16],
}

impl Default for CpuContext {
    fn default() -> Self {
        Self {
            regs: [0; 16],
            cpsr: 0,
            fpscr: 0,
            vfp: [0; 16],
        }
    }
}

impl CpuContext {
    pub fn new() -> Self {
        Self::default()
    }
}

const _: () = {
    assert!(std::mem::offset_of!(CpuContext, cpsr) == 64);
    assert!(std::mem::offset_of!(CpuContext, fpscr) == 68);
    assert!(std::mem::offset_of!(CpuContext, vfp) == 72);
};

#[derive(Debug)]
pub enum CpuState {
    Normal,
    Svc(u32),
    Error(CpuError),
}

#[derive(Debug, Clone, PartialEq)]
pub enum CpuError {
    MemoryError,
    UndefinedInstruction,
    Breakpoint,
}

// Exit codes of n72_enter_guest (>= 0: SVC number).
const EXIT_SLICE: i32 = -1;
const EXIT_MEMORY: i32 = -2;
const EXIT_UNDEFINED: i32 = -3;
const EXIT_BREAKPOINT: i32 = -4;

// State shared with the assembly and the signal handler. Only the host
// thread that runs the guest touches these (touchHLE runs all guest threads
// cooperatively on one host thread).
#[no_mangle]
static mut N72_GUEST_CTX: *mut CpuContext = std::ptr::null_mut();
#[no_mangle]
static mut N72_HOST_SP: u32 = 0;
#[no_mangle]
static mut N72_IN_GUEST: u32 = 0;
#[no_mangle]
static mut N72_SLICE_EXPIRED: u32 = 0;

#[cfg(target_arch = "arm")]
std::arch::global_asm!(
    ".text",
    ".arm",
    ".balign 4",
    ".global n72_asm_start",
    "n72_asm_start:",
    // i32 n72_enter_guest(CpuContext *ctx, const u32 launch[16])
    // launch = ctx->regs with the Thumb bit in launch[15].
    ".global n72_enter_guest",
    ".type n72_enter_guest, %function",
    "n72_enter_guest:",
    "push {{r3-r11, lr}}",
    "vpush {{d8-d15}}",
    "vmrs r2, fpscr",
    "push {{r2, r3}}",
    "ldr r2, ={host_sp}",
    "str sp, [r2]",
    "ldr r2, ={guest_ctx}",
    "str r0, [r2]",
    "add r2, r0, #72",
    "vldmia r2, {{d0-d15}}",
    "ldr r2, [r0, #68]",
    "vmsr fpscr, r2",
    "ldr r2, ={in_guest}",
    "mov r3, #1",
    "str r3, [r2]",
    "ldr r2, [r0, #64]",
    "msr cpsr_fs, r2",
    // Loads r0-r14 and the PC; LDM to PC interworks on ARMv5T and later.
    "ldmia r1, {{r0-r15}}",
    ".size n72_enter_guest, . - n72_enter_guest",
    // Gate: ip = record (svc number, resume PC); all else is guest state.
    ".global n72_common_gate",
    ".type n72_common_gate, %function",
    "n72_common_gate:",
    "str r0, [sp, #-4]",
    "ldr r0, ={guest_ctx}",
    "ldr r0, [r0]",
    "add r0, r0, #4",
    "stmia r0, {{r1-r12}}",
    "ldr r1, [sp, #-4]",
    "str r1, [r0, #-4]",
    "str sp, [r0, #48]",
    "str lr, [r0, #52]",
    "ldr r1, [ip, #4]",
    "str r1, [r0, #56]",
    "mrs r1, cpsr",
    "str r1, [r0, #60]",
    "vmrs r1, fpscr",
    "str r1, [r0, #64]",
    "add r2, r0, #68",
    "vstmia r2, {{d0-d15}}",
    "ldr r0, [ip]",
    // Fall through. Also the signal handler's resume point, r0 = exit code.
    ".global n72_exit_to_host",
    "n72_exit_to_host:",
    "ldr r1, ={in_guest}",
    "mov r2, #0",
    "str r2, [r1]",
    "ldr r1, ={host_sp}",
    "ldr sp, [r1]",
    "pop {{r2, r3}}",
    "vmsr fpscr, r2",
    "vpop {{d8-d15}}",
    "pop {{r3-r11, pc}}",
    ".size n72_common_gate, . - n72_common_gate",
    ".ltorg",
    ".global n72_asm_end",
    "n72_asm_end:",
    host_sp = sym N72_HOST_SP,
    guest_ctx = sym N72_GUEST_CTX,
    in_guest = sym N72_IN_GUEST,
);

#[cfg(target_arch = "arm")]
extern "C" {
    fn n72_enter_guest(ctx: *mut CpuContext, launch: *const u32) -> i32;
    fn n72_common_gate();
    fn n72_exit_to_host();
    static n72_asm_start: u8;
    static n72_asm_end: u8;
}

// Lets the crate type-check on other hosts (e.g. `cargo check` on the Mac);
// the native backend only runs on 32-bit ARM.
#[cfg(not(target_arch = "arm"))]
mod not_arm {
    use super::CpuContext;
    pub unsafe fn n72_enter_guest(_: *mut CpuContext, _: *const u32) -> i32 {
        unimplemented!("the native CPU backend needs a 32-bit ARM host")
    }
    pub extern "C" fn n72_common_gate() {}
}
#[cfg(not(target_arch = "arm"))]
use not_arm::*;

fn common_gate_addr() -> u32 {
    let f: unsafe extern "C" fn() = n72_common_gate;
    f as usize as u32
}

/// Flush the data cache and invalidate the instruction cache for a range of
/// guest (= host) addresses.
pub fn flush_icache(base: VAddr, size: GuestUSize) {
    #[cfg(target_arch = "arm")]
    unsafe {
        // __ARM_NR_cacheflush
        libc::syscall(0x0f0002, base as usize, (base + size) as usize, 0usize);
    }
    #[cfg(not(target_arch = "arm"))]
    let _ = (base, size);
}

pub struct Cpu {
    ctx: CpuContext,
    launch: [u32; 16],
}

impl Cpu {
    pub const SP: usize = 13;
    #[allow(unused)]
    pub const LR: usize = 14;
    pub const PC: usize = 15;

    pub const CPSR_THUMB: u32 = 0x00000020;

    pub const CPSR_USER_MODE: u32 = 0x00000010;

    /// `direct_memory_access` is ignored: guest memory is always host memory.
    pub fn new(_direct_memory_access: Option<&mut Mem>) -> Cpu {
        signals::install();
        Cpu {
            ctx: CpuContext::new(),
            launch: [0; 16],
        }
    }

    pub fn regs(&self) -> &[u32; 16] {
        &self.ctx.regs
    }
    pub fn regs_mut(&mut self) -> &mut [u32; 16] {
        &mut self.ctx.regs
    }

    pub fn dump_regs(&self) {
        Self::echo_regs(self.regs());
    }

    pub fn echo_regs(regs: &[u32; 16]) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for row in 0..4 {
                use std::fmt::Write;
                let mut line = String::new();
                for col in 0..4 {
                    let reg_idx = row * 4 + col;
                    match reg_idx {
                        Self::SP => write!(&mut line, "\t SP: "),
                        Self::LR => write!(&mut line, "\t LR: "),
                        Self::PC => write!(&mut line, "\t PC: "),
                        _ if reg_idx <= 9 => write!(&mut line, "\t R{reg_idx}: "),
                        _ => write!(&mut line, "\tR{reg_idx}: "),
                    }
                    .unwrap();
                    write!(&mut line, "{:#010x}", regs[reg_idx]).unwrap();
                }
                echo!("{}", line);
            }
        }));
    }

    pub fn cpsr(&self) -> u32 {
        self.ctx.cpsr
    }
    pub fn set_cpsr(&mut self, cpsr: u32) {
        self.ctx.cpsr = cpsr;
    }

    /// Swap the current register state with `context`.
    pub fn swap_context(&mut self, context: &mut CpuContext) {
        std::mem::swap(&mut self.ctx, context);
    }

    pub fn pc_with_thumb_bit(&self) -> GuestFunction {
        let pc = self.regs()[Self::PC];
        let thumb = (self.cpsr() & Self::CPSR_THUMB) == Self::CPSR_THUMB;
        GuestFunction::from_addr_and_thumb_flag(pc, thumb)
    }

    pub fn branch(&mut self, new_pc: GuestFunction) {
        self.regs_mut()[Self::PC] = new_pc.addr_without_thumb_bit();
        let cpsr_without_thumb = self.cpsr() & (!Self::CPSR_THUMB);
        self.set_cpsr(cpsr_without_thumb | ((new_pc.is_thumb() as u32) * Self::CPSR_THUMB))
    }

    pub fn branch_with_link(
        &mut self,
        new_pc: GuestFunction,
        new_lr: GuestFunction,
    ) -> (GuestFunction, GuestFunction) {
        let old_pc = self.pc_with_thumb_bit();
        let old_lr = GuestFunction::from_addr_with_thumb_bit(self.regs()[Self::LR]);
        self.branch(new_pc);
        self.regs_mut()[Self::LR] = new_lr.addr_with_thumb_bit();
        (old_pc, old_lr)
    }

    pub fn invalidate_cache_range(&mut self, base: VAddr, size: GuestUSize) {
        flush_icache(base, size);
    }

    /// Run guest code until it calls the host, faults or its time slice
    /// ends (then `*ticks` is set to 0). Single-stepping (`ticks` = None,
    /// used by touchHLE's GDB stub) is not supported: use gdb on the device.
    #[must_use]
    pub fn run_or_step(&mut self, _mem: &mut Mem, ticks: Option<&mut u64>) -> CpuState {
        let Some(ticks) = ticks else {
            panic!("the native CPU backend cannot single-step; debug with gdb on the device");
        };
        self.launch = self.ctx.regs;
        if self.ctx.cpsr & Self::CPSR_THUMB != 0 {
            self.launch[Self::PC] |= 1;
        } else {
            self.launch[Self::PC] &= !3;
        }
        let res = unsafe { n72_enter_guest(&mut self.ctx, self.launch.as_ptr()) };
        unsafe {
            let expired = std::ptr::addr_of_mut!(N72_SLICE_EXPIRED);
            if expired.read_volatile() != 0 {
                expired.write_volatile(0);
                *ticks = 0;
            }
        }
        match res {
            EXIT_SLICE => {
                *ticks = 0;
                CpuState::Normal
            }
            EXIT_MEMORY => CpuState::Error(CpuError::MemoryError),
            EXIT_UNDEFINED => CpuState::Error(CpuError::UndefinedInstruction),
            EXIT_BREAKPOINT => CpuState::Error(CpuError::Breakpoint),
            _ if res < 0 => panic!("unexpected native CPU exit code {res}"),
            svc => CpuState::Svc(svc as u32),
        }
    }
}

// ------------------------------------------------------------------ gates

const ENTRY_SIZE: GuestUSize = 20;
const REGION_SIZE: GuestUSize = 0x10000;
/// A32 `b` reaches ±32 MiB; keep a margin.
const BRANCH_RANGE: i64 = (32 << 20) - 0x1000;

fn encode_a32_b(from: VAddr, to: VAddr) -> u32 {
    let off = (to as i64 - (from as i64 + 8)) >> 2;
    assert!((-(1 << 23)..(1 << 23)).contains(&off));
    0xea00_0000 | (off as u32 & 0x00ff_ffff)
}

struct Region {
    base: VAddr,
    next: VAddr,
}

/// Gate entries and small host-call routines, per guest address space
/// (owned by [Mem]).
#[derive(Default)]
pub struct Gates {
    regions: Vec<Region>,
    /// Call site → its gate entry.
    sites: HashMap<VAddr, VAddr>,
}

impl Gates {
    fn alloc_near(&mut self, mem: &mut Mem, size: GuestUSize, near: Option<VAddr>) -> VAddr {
        let in_range = |addr: VAddr| match near {
            None => true,
            Some(near) => (addr as i64 - near as i64).abs() < BRANCH_RANGE,
        };
        for r in &mut self.regions {
            if r.next + size <= r.base + REGION_SIZE && in_range(r.next) {
                let addr = r.next;
                r.next += (size + 3) & !3;
                return addr;
            }
        }
        let base = match near {
            None => mem.vm_alloc(None, REGION_SIZE).unwrap().to_bits(),
            Some(near) => {
                // Try 1 MiB steps above the site, then below it.
                let mb = near & !0xf_ffff;
                (1..=30)
                    .map(|k| mb.wrapping_add(k << 20))
                    .chain((1..=30).map(|k| mb.wrapping_sub(k << 20)))
                    .filter(|&a| a >= 0x10_0000 && a < 0xf000_0000 && in_range(a))
                    .find_map(|a| mem.vm_alloc(Some(a), REGION_SIZE).ok())
                    .unwrap_or_else(|| panic!("no room for a gate region near {near:#x}"))
                    .to_bits()
            }
        };
        log_dbg!("New gate region at {:#x} (near {:x?})", base, near);
        self.regions.push(Region { base, next: base });
        self.alloc_near(mem, size, near)
    }
}

/// Write an instruction at `site` that calls into the host with SVC number
/// `svc` and then continues at `site + 4` (see the module documentation).
pub fn write_svc(mem: &mut Mem, site: VAddr, svc: u32) {
    let mut gates = std::mem::take(mem.native_gates_mut());
    let entry = match gates.sites.get(&site) {
        Some(&entry) => entry,
        None => {
            let entry = gates.alloc_near(mem, ENTRY_SIZE, Some(site));
            let words = [
                0xe28f_c004,        // add ip, pc, #4
                0xe51f_f004,        // ldr pc, [pc, #-4]
                common_gate_addr(), // n72_common_gate
                0,                  // svc
                site + 4,           // resume PC
            ];
            for (i, w) in words.iter().enumerate() {
                let p: MutPtr<u32> = Ptr::from_bits(entry + 4 * i as u32);
                mem.write(p, *w);
            }
            gates.sites.insert(site, entry);
            entry
        }
    };
    mem.write(Ptr::from_bits(entry + 12), svc);
    flush_icache(entry, ENTRY_SIZE);
    mem.write(Ptr::from_bits(site), encode_a32_b(site, entry));
    flush_icache(site, 4);
    *mem.native_gates_mut() = gates;
}

/// Allocate guest memory for a small routine that contains SVC sites
/// (placed in a gate region, so its own gate entries are in range).
/// n72 direct calls: 1 while the host GL context that is current belongs to
/// the running guest thread (see `crate::n72_direct`). Read by the direct
/// trampolines; anything else makes them take the gate.
pub static mut N72_GL_DIRECT_READY: u32 = 0;
/// The shared parts of the direct trampolines: (ready flag's address, routine).
static N72_DIRECT_COMMON: std::sync::Mutex<Vec<(u32, VAddr)>> = std::sync::Mutex::new(Vec::new());

/// n72 direct calls: make the stub at `site` (already linked to a gate with
/// [write_svc]) call `host_fn` straight from guest code when `ready` (an
/// address holding a u32) is non-zero, else take the gate as before.
/// `host_fn` must use the guest's calling convention (AAPCS, soft-float),
/// take its arguments in registers only (two words go on the guest's stack
/// first), and never re-enter the guest.
///
/// A stub of four words per function leads to a routine all of them share
/// (per ready flag): the calls come by the thousand per frame, between
/// stretches of guest code that push everything else out of the 16 KiB
/// instruction cache, so what each function's own path adds must be small.
/// (A trampoline of its own was 80 bytes, three cache lines, with literals
/// that cost data cache lines as well.) Host functions reached this way are
/// not preempted: N72_IN_GUEST is 0 meanwhile.
pub fn write_direct(mem: &mut Mem, site: VAddr, host_fn: u32, ready: u32) -> bool {
    let mut gates = std::mem::take(mem.native_gates_mut());
    let Some(&gate) = gates.sites.get(&site) else {
        *mem.native_gates_mut() = gates;
        return false;
    };
    let in_range = |a: VAddr, b: VAddr| (a as i64 - b as i64).abs() < BRANCH_RANGE - 0x1000;
    let mut commons = N72_DIRECT_COMMON.lock().unwrap();
    let common = match commons.iter().find(|&&(r, c)| r == ready && in_range(c, site)) {
        Some(&(_, c)) => c,
        None => {
            let c = gates.alloc_near(mem, 20 * 4, Some(site));
            let off = |i: u32, lit: u32| -> u32 { 4 * lit - (4 * i + 8) };
            let in_guest = std::ptr::addr_of!(N72_IN_GUEST) as u32;
            // On entry ip = the stub's third word (its branch to the gate);
            // the host function's address follows it. Darwin/iOS guest stacks
            // are only 4-byte aligned, so align sp to 8 bytes for Linux AAPCS
            // before calling into host GL/libc/libm.
            let words: [u32; 20] = [
                0xe92d_4030,              // push {r4, r5, lr}
                0xe59f_4000 | off(1, 18), // ldr r4, =ready
                0xe594_e000,              // ldr lr, [r4]
                0xe35e_0000,              // cmp lr, #0
                0x0a00_000a,              // beq 16f
                0xe1a0_500d,              // mov r5, sp
                0xe3cd_d007,              // bic sp, sp, #7
                0xe59f_4000 | off(7, 19), // ldr r4, =N72_IN_GUEST
                0xe3a0_e000,              // mov lr, #0
                0xe584_e000,              // str lr, [r4]
                0xe59c_c004,              // ldr ip, [ip, #4]
                0xe12f_ff3c,              // blx ip
                0xe3a0_c001,              // mov ip, #1
                0xe584_c000,              // str ip, [r4]
                0xe1a0_d005,              // mov sp, r5
                0xe8bd_8030,              // pop {r4, r5, pc}
                0xe8bd_4030,              // 16: pop {r4, r5, lr}
                0xe12f_ff1c,              // bx ip (the stub's branch to the gate)
                ready,
                in_guest,
            ];
            for (i, w) in words.iter().enumerate() {
                mem.write(Ptr::from_bits(c + 4 * i as u32), *w);
            }
            flush_icache(c, 20 * 4);
            commons.push((ready, c));
            c
        }
    };
    let stub = gates.alloc_near(mem, 4 * 4, Some(site));
    *mem.native_gates_mut() = gates;
    let words: [u32; 4] = [
        0xe1a0_c00f,                   // mov ip, pc
        encode_a32_b(stub + 4, common), // b common
        encode_a32_b(stub + 8, gate),   // b gate
        host_fn,
    ];
    for (i, w) in words.iter().enumerate() {
        mem.write(Ptr::from_bits(stub + 4 * i as u32), *w);
    }
    flush_icache(stub, 4 * 4);
    mem.write(Ptr::from_bits(site), encode_a32_b(site, stub));
    flush_icache(site, 4);
    true
}

pub fn alloc_code(mem: &mut Mem, size: GuestUSize) -> VAddr {
    let mut gates = std::mem::take(mem.native_gates_mut());
    let addr = gates.alloc_near(mem, size, None);
    *mem.native_gates_mut() = gates;
    addr
}

// ---------------------------------------------------------------- signals

mod signals {
    #[cfg(target_arch = "arm")]
    use super::*;
    use std::sync::Once;

    static INSTALL: Once = Once::new();

    /// Time slice length for preempting guest threads.
    #[cfg(target_arch = "arm")]
    const SLICE_NS: libc::c_long = 10_000_000;

    #[cfg(not(target_arch = "arm"))]
    pub fn install() {
        INSTALL.call_once(|| {});
    }

    // Sampling profiler (env N72_PROF=/path): each time-slice tick records
    // the interrupted PC (bit 0 = Thumb) and LR of the guest thread, which is
    // a wall-clock profile (blocked syscalls show at their libc call site).
    // SIGUSR2, or a full buffer, writes the pairs to the file as raw u32s.
    #[cfg(all(target_arch = "arm", feature = "n72-prof"))]
    mod prof {
        pub const MAX: usize = 60_000;
        pub static mut ON: u32 = 0;
        pub static mut N: usize = 0;
        /// Ticks (10 ms) still to skip before sampling starts (N72_PROF_SKIP=seconds):
        /// keeps loading screens out of a profile of the game itself.
        pub static mut SKIP: u32 = 0;
        pub static mut DUMPED: u32 = 0;
        pub static mut BUF: [u32; MAX * 2] = [0; MAX * 2];
        pub static mut PATH: [u8; 128] = [0; 128];

        pub unsafe fn dump() {
            let fd = libc::open(
                std::ptr::addr_of!(PATH) as *const libc::c_char,
                libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
                0o644,
            );
            if fd >= 0 {
                let n = std::ptr::addr_of!(N).read_volatile();
                libc::write(fd, std::ptr::addr_of!(BUF) as *const _, n * 8);
                libc::close(fd);
            }
        }

        pub unsafe extern "C" fn dump_handler(_sig: libc::c_int) {
            dump();
        }
    }

    #[cfg(target_arch = "arm")]
    fn timer_signal() -> libc::c_int {
        libc::SIGRTMIN() + 3
    }

    #[cfg(target_arch = "arm")]
    pub fn install() {
        INSTALL.call_once(|| unsafe {
            // Alternate stack: the guest stack may be the one that faulted.
            const ALT_SIZE: usize = 64 * 1024;
            let alt = libc::mmap(
                std::ptr::null_mut(),
                ALT_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            assert!(alt != libc::MAP_FAILED);
            let ss = libc::stack_t {
                ss_sp: alt,
                ss_flags: 0,
                ss_size: ALT_SIZE,
            };
            assert!(libc::sigaltstack(&ss, std::ptr::null_mut()) == 0);

            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = handler as *const () as usize;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_RESTART;
            // The handler must not be interrupted by another of these: a
            // nested one would see N72_IN_GUEST still set and take the
            // handler's own registers for the guest's.
            let sigs = [
                libc::SIGSEGV,
                libc::SIGBUS,
                libc::SIGILL,
                libc::SIGTRAP,
                timer_signal(),
            ];
            libc::sigemptyset(&mut sa.sa_mask);
            for sig in sigs {
                libc::sigaddset(&mut sa.sa_mask, sig);
            }
            for sig in sigs {
                assert!(libc::sigaction(sig, &sa, std::ptr::null_mut()) == 0);
            }

            #[cfg(feature = "n72-prof")]
            if let Ok(path) = std::env::var("N72_PROF") {
                let b = path.as_bytes();
                if b.len() < 127 {
                    let dst = &mut *std::ptr::addr_of_mut!(prof::PATH);
                    dst[..b.len()].copy_from_slice(b);
                    std::ptr::addr_of_mut!(prof::ON).write_volatile(1);
                    if let Some(secs) = std::env::var("N72_PROF_SKIP").ok().and_then(|v| v.parse::<u32>().ok()) {
                        std::ptr::addr_of_mut!(prof::SKIP).write_volatile(secs * 100);
                    }
                    libc::signal(libc::SIGUSR2, prof::dump_handler as *const () as usize);
                    log!(
                        "n72 profiler on: kill -USR2 {} writes {}",
                        libc::getpid(),
                        path
                    );
                }
            }

            // Periodic timer aimed at this thread (the one running guests).
            let mut sev: libc::sigevent = std::mem::zeroed();
            sev.sigev_notify = 4; // SIGEV_THREAD_ID (not exported by libc for musl)
            sev.sigev_signo = timer_signal();
            sev.sigev_notify_thread_id = libc::gettid();
            let mut timer: libc::timer_t = std::mem::zeroed();
            assert!(libc::timer_create(libc::CLOCK_MONOTONIC, &mut sev, &mut timer) == 0);
            let period = libc::timespec {
                tv_sec: 0,
                tv_nsec: SLICE_NS,
            };
            let its = libc::itimerspec {
                it_interval: period,
                it_value: period,
            };
            assert!(libc::timer_settime(timer, 0, &its, std::ptr::null_mut()) == 0);
        });
    }

    /// Offset of the VFP frame's registers in `uc_regspace` (struct
    /// vfp_sigframe: magic, size, then user_vfp { u64 fpregs[32]; u32 fpscr }).
    #[cfg(target_arch = "arm")]
    const VFP_MAGIC: u32 = 0x5646_5001;

    #[cfg(target_arch = "arm")]
    unsafe extern "C" fn handler(
        sig: libc::c_int,
        _info: *mut libc::siginfo_t,
        uc: *mut libc::c_void,
    ) {
        let uc = &mut *(uc as *mut libc::ucontext_t);
        let mc = &mut uc.uc_mcontext;
        let pc = mc.arm_pc as usize;
        let asm = (&n72_asm_start as *const u8 as usize)..(&n72_asm_end as *const u8 as usize);
        let in_guest = std::ptr::addr_of!(N72_IN_GUEST).read_volatile() != 0 && !asm.contains(&pc);

        #[cfg(feature = "n72-prof")]
        if sig == timer_signal() && std::ptr::addr_of!(prof::ON).read_volatile() != 0 {
            let n = std::ptr::addr_of!(prof::N).read_volatile();
            let skip = std::ptr::addr_of!(prof::SKIP).read_volatile();
            if skip > 0 {
                std::ptr::addr_of_mut!(prof::SKIP).write_volatile(skip - 1);
            } else if n < prof::MAX {
                let buf = &mut *std::ptr::addr_of_mut!(prof::BUF);
                buf[2 * n] = mc.arm_pc as u32 | ((mc.arm_cpsr as u32 >> 5) & 1);
                buf[2 * n + 1] = mc.arm_lr as u32;
                std::ptr::addr_of_mut!(prof::N).write_volatile(n + 1);
            } else if std::ptr::addr_of!(prof::DUMPED).read_volatile() == 0 {
                std::ptr::addr_of_mut!(prof::DUMPED).write_volatile(1);
                prof::dump();
            }
        }
        let code = if sig == timer_signal() {
            std::ptr::addr_of_mut!(N72_SLICE_EXPIRED).write_volatile(1);
            if !in_guest {
                return;
            }
            EXIT_SLICE
        } else {
            if !in_guest {
                // A host crash: report where (async-signal-safe), then let it
                // happen as it would without us.
                let mut buf = [0u8; 160];
                let mut n = 0;
                let mut put = |s: &[u8]| {
                    for &b in s {
                        if n < buf.len() {
                            buf[n] = b;
                            n += 1;
                        }
                    }
                };
                let hex = |v: u32| {
                    let mut h = [0u8; 10];
                    h[0] = b'0';
                    h[1] = b'x';
                    for i in 0..8 {
                        let d = ((v >> (28 - 4 * i)) & 0xf) as u8;
                        h[2 + i] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
                    }
                    h
                };
                put(b"n72 native: host crash, signal ");
                put(&[b'0' + (sig / 10) as u8, b'0' + (sig % 10) as u8]);
                put(b" pc ");
                put(&hex(mc.arm_pc as u32));
                put(b" lr ");
                put(&hex(mc.arm_lr as u32));
                put(b" addr ");
                put(&hex(mc.fault_address as u32));
                put(b"\n");
                libc::write(2, buf.as_ptr() as *const _, n);
                libc::signal(sig, libc::SIG_DFL);
                return;
            }
            // ARM Linux reports a user BKPT (a prefetch abort) as SIGBUS, so
            // look at the instruction.
            let thumb = mc.arm_cpsr as u32 & Cpu::CPSR_THUMB != 0;
            let is_bkpt = if thumb {
                (*(pc as *const u16) & 0xff00) == 0xbe00
            } else {
                (*(pc as *const u32) & 0x0ff0_00f0) == 0x0120_0070
            };
            match sig {
                _ if is_bkpt && sig != libc::SIGSEGV => EXIT_BREAKPOINT,
                libc::SIGILL => EXIT_UNDEFINED,
                libc::SIGTRAP => EXIT_BREAKPOINT,
                _ => EXIT_MEMORY,
            }
        };

        let ctx = &mut *std::ptr::addr_of!(N72_GUEST_CTX).read_volatile();
        ctx.regs = [
            mc.arm_r0 as u32,
            mc.arm_r1 as u32,
            mc.arm_r2 as u32,
            mc.arm_r3 as u32,
            mc.arm_r4 as u32,
            mc.arm_r5 as u32,
            mc.arm_r6 as u32,
            mc.arm_r7 as u32,
            mc.arm_r8 as u32,
            mc.arm_r9 as u32,
            mc.arm_r10 as u32,
            mc.arm_fp as u32,
            mc.arm_ip as u32,
            mc.arm_sp as u32,
            mc.arm_lr as u32,
            mc.arm_pc as u32,
        ];
        ctx.cpsr = mc.arm_cpsr as u32;
        // Like dynarmic: after an undefined instruction or breakpoint the PC
        // points past it (Environment::debug_cpu_error steps back).
        if code == EXIT_UNDEFINED || code == EXIT_BREAKPOINT {
            ctx.regs[15] += if ctx.cpsr & Cpu::CPSR_THUMB != 0 {
                2
            } else {
                4
            };
        }
        let space = uc.uc_regspace.as_ptr() as *const u32;
        let mut off = 0usize;
        while off + 2 < uc.uc_regspace.len() * 2 {
            let magic = *space.add(off);
            let size = *space.add(off + 1) as usize;
            if magic == VFP_MAGIC {
                let regs = space.add(off + 2) as *const u64;
                for i in 0..16 {
                    ctx.vfp[i] = regs.add(i).read_unaligned();
                }
                ctx.fpscr = *(space.add(off + 2 + 64));
                break;
            }
            if magic == 0 || size == 0 {
                break;
            }
            off += size / 4;
        }

        // Resume in the host: n72_exit_to_host with r0 = exit code, ARM state.
        mc.arm_r0 = code as u32 as libc::c_ulong;
        mc.arm_pc = (n72_exit_to_host as *const () as usize) as libc::c_ulong;
        mc.arm_cpsr &= !(Cpu::CPSR_THUMB as libc::c_ulong | 0x0600_fc00);
    }
}
