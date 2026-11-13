/*
 * This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/.
 */
//! Self-test for the native CPU backend, run on the device with
//! `touchHLE --n72-native-selftest`. It uses hand-assembled guest code, so it
//! needs no app, frameworks or display.

use super::native::{write_svc, Cpu, CpuError, CpuState};
use crate::mem::{Mem, MutPtr, Ptr, VAddr};

const SVC_CALL: u32 = 7;
const SVC_DONE: u32 = 2;

fn put(mem: &mut Mem, addr: VAddr, words: &[u32]) {
    for (i, w) in words.iter().enumerate() {
        let p: MutPtr<u32> = Ptr::from_bits(addr + 4 * i as u32);
        mem.write(p, *w);
    }
}

fn put16(mem: &mut Mem, addr: VAddr, halves: &[u16]) {
    for (i, h) in halves.iter().enumerate() {
        let p: MutPtr<u16> = Ptr::from_bits(addr + 2 * i as u32);
        mem.write(p, *h);
    }
}

struct T {
    fails: u32,
}
impl T {
    fn check(&mut self, ok: bool, what: &str) {
        echo!("{}  {}", if ok { "PASS" } else { "FAIL" }, what);
        self.fails += (!ok) as u32;
    }
}

/// Run until the guest returns to the "done" routine, servicing SVC_CALL by
/// multiplying r0 by 10. Returns the final state and how many calls happened.
fn run(cpu: &mut Cpu, mem: &mut Mem, budget: u32) -> (CpuState, u32) {
    let mut calls = 0;
    for _ in 0..budget {
        let mut ticks = 100_000u64;
        match cpu.run_or_step(mem, Some(&mut ticks)) {
            CpuState::Svc(SVC_CALL) => {
                calls += 1;
                cpu.regs_mut()[0] *= 10;
                // VFP state is preserved across the host call even if the
                // host uses the VFP registers.
                let x: f64 = std::hint::black_box(1.5f64) * std::hint::black_box(3.0);
                std::hint::black_box(x);
            }
            CpuState::Normal if ticks == 0 => continue,
            other => return (other, calls),
        }
    }
    (CpuState::Normal, calls)
}

pub fn run_selftest() -> i32 {
    let mut t = T { fails: 0 };
    let mut mem = Mem::new();
    let mut cpu = Cpu::new(Some(&mut mem));

    // Layout inside one 64 KiB chunk near the bottom of the arena.
    let base = mem.vm_alloc(None, 0x10000).unwrap().to_bits();
    let (done, site, arm_fn, thumb_fn, stack_top) = (
        base,
        base + 0x100,
        base + 0x200,
        base + 0x400,
        base + 0x10000,
    );

    // "Return to host" routine: site with SVC_DONE, then a trap.
    put(&mut mem, done, &[0, 0xe7ff_defe]);
    write_svc(&mut mem, done, SVC_DONE);
    // A host function stub: SVC_CALL; bx lr.
    put(&mut mem, site, &[0, 0xe12f_ff1e]);
    write_svc(&mut mem, site, SVC_CALL);

    // ARM guest: r0 = 5; call site; r0 += 1; d8 = 2.0 survives the call;
    // returns r0 + (d8 == 2.0 ? 100 : 0). (VFPv2 has no vmov immediate, so
    // 2.0 = 0x40000000_00000000 goes in through core registers.)
    put(
        &mut mem,
        arm_fn,
        &[
            0xe92d_4000,                                                             // push {lr}
            0xe3a0_1101, // mov r1, #0x40000000
            0xe3a0_2000, // mov r2, #0
            0xec41_2b18, // vmov d8, r2, r1
            0xe3a0_0005, // mov r0, #5
            0xeb00_0000 | ((site.wrapping_sub(arm_fn + 20 + 8) >> 2) & 0x00ff_ffff), // bl site
            0xe280_0001, // add r0, r0, #1
            0xe3a0_1101, // mov r1, #0x40000000
            0xe3a0_2000, // mov r2, #0
            0xec41_2b17, // vmov d7, r2, r1
            0xeeb4_8b47, // vcmp.f64 d8, d7
            0xeef1_fa10, // vmrs APSR_nzcv, fpscr
            0x0280_0064, // addeq r0, r0, #100
            0xe8bd_8000, // pop {pc}
        ],
    );
    let regs = cpu.regs_mut();
    regs[13] = stack_top;
    regs[14] = done;
    cpu.branch(crate::abi::GuestFunction::from_addr_with_thumb_bit(arm_fn));
    flush(arm_fn, 64);
    let (state, calls) = run(&mut cpu, &mut mem, 10);
    t.check(
        matches!(state, CpuState::Svc(SVC_DONE)) && calls == 1,
        &format!(
            "ARM guest calls the host once and returns to the host routine ({:?}, {} calls, pc {:#x})",
            state,
            calls,
            cpu.regs()[15]
        ),
    );
    t.check(
        cpu.regs()[0] == 151,
        &format!(
            "ARM result r0 = {} (expect 151: 5*10+1, d8 kept)",
            cpu.regs()[0]
        ),
    );
    t.check(cpu.regs()[15] == done + 4, "PC after an SVC is site + 4");

    // Thumb guest: r0 = 3; blx site; r0 += 2; pop {pc} -> 32.
    let blx_off = site.wrapping_sub((thumb_fn + 4 + 4) & !3) as i32;
    put16(
        &mut mem,
        thumb_fn,
        &[
            0xb500, // push {lr}
            0x2003, // movs r0, #3
            0, 0,      // blx site (below)
            0x3002, // adds r0, #2
            0xbd00, // pop {pc}
        ],
    );
    let (hi, lo) = encode_t1_blx(blx_off);
    put16(&mut mem, thumb_fn + 4, &[hi, lo]);
    flush(thumb_fn, 16);
    let regs = cpu.regs_mut();
    regs[13] = stack_top;
    regs[14] = done;
    cpu.branch(crate::abi::GuestFunction::from_addr_with_thumb_bit(
        thumb_fn | 1,
    ));
    let (state, calls) = run(&mut cpu, &mut mem, 10);
    t.check(
        matches!(state, CpuState::Svc(SVC_DONE)) && calls == 1 && cpu.regs()[0] == 32,
        &format!("Thumb guest via blx: r0 = {} (expect 32)", cpu.regs()[0]),
    );

    // Faults: a null read, an undefined instruction, a breakpoint.
    let fault_fn = base + 0x600;
    for (code, expect, name) in [
        (
            0xe3a0_1000u32,
            CpuError::MemoryError,
            "null read -> MemoryError",
        ), // mov r1,#0 then ldr
        (
            0xe7f0_00f0,
            CpuError::UndefinedInstruction,
            "udf -> UndefinedInstruction",
        ),
        (0xe120_0070, CpuError::Breakpoint, "bkpt -> Breakpoint"),
    ] {
        let words: &[u32] = if expect == CpuError::MemoryError {
            &[code, 0xe591_0000] // ldr r0, [r1]
        } else {
            &[code]
        };
        put(&mut mem, fault_fn, words);
        flush(fault_fn, 8);
        let regs = cpu.regs_mut();
        regs[13] = stack_top;
        regs[14] = done;
        cpu.branch(crate::abi::GuestFunction::from_addr_with_thumb_bit(
            fault_fn,
        ));
        let mut ticks = 100_000u64;
        let state = cpu.run_or_step(&mut mem, Some(&mut ticks));
        t.check(
            matches!(&state, CpuState::Error(e) if *e == expect),
            &format!("{name} ({state:?}, pc {:#x})", cpu.regs()[15]),
        );
    }

    // Preemption: an endless loop must come back with the slice used up.
    let spin = base + 0x700;
    put(&mut mem, spin, &[0xeaff_fffe]); // b .
    flush(spin, 4);
    cpu.regs_mut()[13] = stack_top;
    cpu.branch(crate::abi::GuestFunction::from_addr_with_thumb_bit(spin));
    let mut ticks = 100_000u64;
    let state = cpu.run_or_step(&mut mem, Some(&mut ticks));
    t.check(
        matches!(state, CpuState::Normal) && ticks == 0 && cpu.regs()[15] == spin,
        "endless loop is preempted by the time slice",
    );

    // Preemption keeps guest state: a long countdown loop is interrupted by
    // the time slice several times; d9 (VFP), r4 and the flags must survive.
    let pre = base + 0x800;
    put(
        &mut mem,
        pre,
        &[
            0xe92d_4010, // push {r4, lr}
            0xe3a0_1101, // mov r1, #0x40000000
            0xe3a0_2000, // mov r2, #0
            0xec41_2b19, // vmov d9, r2, r1         (2.0)
            0xe3a0_4c7d, // mov r4, #0x7d00
            0xe3a0_3640, // mov r3, #0x4000000      (64 M iterations)
            0xe253_3001, // loop: subs r3, r3, #1
            0x1aff_fffd, // bne loop
            0xec41_2b17, // vmov d7, r2, r1
            0xeeb4_9b47, // vcmp.f64 d9, d7
            0xeef1_fa10, // vmrs APSR_nzcv, fpscr
            0x03a0_0001, // moveq r0, #1
            0x13a0_0002, // movne r0, #2
            0xe354_0c7d, // cmp r4, #0x7d00
            0x13a0_0003, // movne r0, #3
            0xe8bd_8010, // pop {r4, pc}
        ],
    );
    flush(pre, 64);
    let regs = cpu.regs_mut();
    regs[13] = stack_top;
    regs[14] = done;
    cpu.branch(crate::abi::GuestFunction::from_addr_with_thumb_bit(pre));
    let mut slices = 0;
    let state = loop {
        let mut ticks = 100_000u64;
        match cpu.run_or_step(&mut mem, Some(&mut ticks)) {
            CpuState::Normal if ticks == 0 => slices += 1,
            other => break other,
        }
    };
    t.check(
        matches!(state, CpuState::Svc(SVC_DONE)) && cpu.regs()[0] == 1 && slices > 0,
        &format!(
            "preempted {} times; VFP, r4 and flags kept: r0 = {} (1 = ok, 2 = d9 lost, 3 = r4 lost), {:?}",
            slices,
            cpu.regs()[0],
            state
        ),
    );

    // Host-call round trip cost.
    put(
        &mut mem,
        arm_fn,
        &[
            0xeb00_0000 | ((site.wrapping_sub(arm_fn + 8) >> 2) & 0x00ff_ffff),
            0xeaff_fffd,
        ],
    ); // bl site; b arm_fn
    flush(arm_fn, 8);
    cpu.regs_mut()[13] = stack_top;
    cpu.branch(crate::abi::GuestFunction::from_addr_with_thumb_bit(arm_fn));
    let n = 200_000;
    let start = std::time::Instant::now();
    let mut seen = 0;
    while seen < n {
        let mut ticks = 100_000u64;
        if let CpuState::Svc(SVC_CALL) = cpu.run_or_step(&mut mem, Some(&mut ticks)) {
            seen += 1;
        }
    }
    let ns = start.elapsed().as_nanos() / n as u128;
    echo!("      host call round trip: {} ns", ns);

    echo!("RESULT {} failure(s)", t.fails);
    t.fails as i32
}

fn flush(addr: VAddr, size: u32) {
    super::native::flush_icache(addr, size);
}

/// Thumb-1 BLX (immediate) pair for a byte offset from Align(PC, 4).
fn encode_t1_blx(off: i32) -> (u16, u16) {
    let off = off as u32;
    let hi = 0xf000 | ((off >> 12) & 0x7ff) as u16;
    let lo = 0xe800 | ((off >> 1) & 0x7fe) as u16;
    (hi, lo)
}
