# touchHLE with a native CPU backend on the iPod touch 2G (N72)

Goal: run iPhone OS 2–4 armv6 apps under the Alpine Linux root, with app code
executing directly on the ARM1176 and touchHLE's reimplemented frameworks
(UIKit, Foundation, OpenGL ES, OpenAL…) running as native Rust code in the
same process.

Based on upstream touchHLE `ca1de89` (v0.3.0-11), cloned to `touchHLE/`.

## Status, 2026-11-13 (device-tested on kernel #26)

| Piece | Where | State |
| --- | --- | --- |
| Kernel: user mappings from 0x1000 (`CONFIG_ARM_LOW_USER_MAPPINGS`) | linux-s5l87xx `n72` 2bc56dd3f (kernel #26, booted and staged in alpine-work/kx) | Device: 0x1000 mappable, code runs there, page 0 refused, null reads fault |
| Fork: native CPU backend, identity Mem, gates, fixed portrait panel, evdev accelerometer, `--n72-native-selftest` | touchHLE branch `n72-native` (fd7b839, 05ffde9) | Self-test 8/8 on the device (calls ARM/Thumb, resume PC, VFP kept, null/udf/bkpt faults, preemption), arena at 0x1000; host call round trip ≈ 510 ns |
| n72-gles: `GL_OES_framebuffer_object` (+ rgb8_rgba8, depth24), `GL_EXT_texture_format_BGRA8888` | n72-gles ec2e835, fc78e04 | Host 165/165; MBX `tests/fbo_device.c` 16/16. Not installed system-wide yet (tested with LD_LIBRARY_PATH) |

Device facts found: a user `bkpt` arrives as SIGBUS (prefetch abort), the
trap word 0xe7ffdefe as SIGILL; the ARM1176's VFPv2 has no `vmov #imm`.

Next: install the new n72-gles (apk) after checking n72-ui with it, then
touchHLE's app picker on the panel, then TestApp.

Build the fork: `. n72/env.sh && cargo build --release --target
arm-unknown-linux-musleabihf --no-default-features --features cpu-native`
(needs `alpine-work/sysroot-armhf`). On the device: Alpine `sdl2`,
`openal-soft-libs`, `libstdc++`, `libgcc` (installed), plus the new n72-gles.

## 0. Ground rules for the fork

- **Private fork, never upstreamed.** touchHLE's CONTRIBUTING.md forbids
  AI-generated code, and its AGENTS.md/CLAUDE.md are a deliberate deterrent
  to coding agents. MPL-2.0 permits a modified fork. Our changes stay out of
  their tracker and Gerrit.
- **Rename it** (working name `n72hle`) so it isn't presented as touchHLE.
- **Keep MPL-2.0 headers** on touchHLE files. If a build is ever shared, its
  source (at least the modified MPL files) goes with it.
- **Decrypted apps only**, the same rule touchHLE has.

## 1. What the survey found

| Piece | touchHLE today | Native consequence |
| --- | --- | --- |
| CPU (`src/cpu.rs`, 302 lines) | Thin wrapper over dynarmic: `regs`, `cpsr`, `swap_context`, `run_or_step(ticks)` → `Normal / Svc(n) / Error` | Replace only this API. The environment's scheduler and HLE dispatch can stay as they are. |
| Guest ABI (`src/abi.rs`) | Host functions read arguments from the saved register array (softfp: `f32` from a core register) | **No per-function calling-convention thunks are needed.** The Darwin APCS/softfp vs armhf mismatch is already handled, because we hand Rust the saved guest registers. |
| Host calls (`src/dyld.rs`) | Stubs rewritten to `svc #n` (+ `bx lr`) through `encode_a32_svc`, 5 call sites; ids 0–2 reserved (lazy link, thread exit, return-to-host), `0x800000` flag = "return after" | A real `svc` traps into Linux, so these sites must become branches into a gate (§3.2). |
| Memory (`src/mem.rs`, `mem/host.rs`) | `bytes: *mut [u8; 1<<32]`, a 4 GiB host mmap; guest pointer = offset; main stack at the top of 4 GiB (`0u32 - 1 MiB`) | On a 32-bit host the guest address must equal the host address: identity mapping with base 0, an arena in the low user range, and the stack moved down (§3.1). |
| Threads (`environment.rs`) | Cooperative guest threads on one host thread; `corosensei` coroutines for the host side; preemption via a dynarmic tick budget (100 000) | Keep the cooperative model. Replace ticks with a timer signal (§3.4). Check that `corosensei` builds for ARMv6 (its ARM assembly may use ARMv7-only instructions). |
| GL (`src/gles/`) | `gles1_native.rs` passes GLES 1.1 straight to a host GLES1 context (used on Android); `gles1_on_gl2.rs` for desktops | Use `gles1_native` on top of `libn72gles` (MBX). This is the main reason the port is attractive. |
| Window/input/audio | rust-sdl2 fork (`touchHLE-3`), statically bundled SDL; vendored openal-soft | Link the device's SDL2 2.26.5 (kmsdrm + GLES1) and Alpine's openal-soft dynamically. |

## 2. Phase 0: host build on armhf without a CPU (2–4 days)

1. Copy to `n72hle/` and add Cargo features `cpu-dynarmic` (default on the
   Mac) and `cpu-native` (armhf). Under `cpu-native`, compile out
   `touchHLE_dynarmic_wrapper`; dynarmic has no ARM32 host backend.
2. Add the target `arm-unknown-linux-musleabihf` (ARMv6 + VFP2, hard-float,
   musl; matches Alpine armhf). Build inside the `n72-alpine` Docker image
   with the existing clang/lld + armhf sysroot. Cargo linker = the
   `armhf-cc` wrapper.
3. Turn off the `static` feature. Use the system SDL2 and openal-soft
   (`apk`: `sdl2` is our own 2.26.5 build; `openal-soft` from Alpine). Check
   that the rust-sdl2 fork's API expectations fit SDL 2.26.5.
4. Force the `gles1_native` backend. Context: SDL kmsdrm → EGL/GBM →
   `libn72gles`.
5. **Milestone:** on the device, `n72hle` starts with `n72-ui` stopped, and
   the app picker draws on the MBX and takes touch input.

## 3. Phase 1–2: native CPU backend (≈1–2 weeks)

New module `src/cpu/native/` (Rust + one `.S` file), selected by
`cpu-native`, exposing the same `Cpu` API.

### 3.1 Memory: identity mapping

- At startup, reserve a guest arena with
  `mmap(0x1000, ARENA_END-0x1000, PROT_READ|WRITE|EXEC,
  MAP_PRIVATE|MAP_ANONYMOUS|MAP_NORESERVE|MAP_FIXED_NOREPLACE)`. Choose
  `ARENA_END` (around 0x6000_0000) from the device's `/proc/<pid>/maps`.
  The PIE executable, musl, SDL and GL libraries must sit above it.
- `Mem.bytes` becomes base pointer 0, accessed through raw pointer
  arithmetic. The `[u8; 1<<32]` type cannot exist when `usize` is 32 bits.
  Reads and writes in `Mem` become plain loads and stores.
- Leave `__PAGEZERO` truly unmapped, so app null dereferences fault for
  real.
- Move `MAIN_THREAD_STACK_LOW_END` and the `VMAllocator` upper bound to the
  arena top.
- Kernel checks: `vm.mmap_min_addr` ≤ 4096 (pre-4.3 apps load `__TEXT` at
  0x1000), and the 3G/1G user split.
- Bonus: guest pointers passed to GL (`glTexImage2D` and similar) are now
  real host pointers.

### 3.2 Entering and leaving guest code

- `CpuContext`: `regs[16]`, `cpsr` (only the T bit and flags matter),
  `fpscr`, `d0–d15` (ARM1176 VFPv2).
- `run_or_step` → `n72_enter_guest(ctx)` in assembly:
  1. Save the host's callee-saved state (r4–r11, sp, lr, d8–d15, fpscr) in
     a per-thread host frame.
  2. Load all guest registers.
  3. `bx` to the PC, with the Thumb bit taken from the CPSR.
- **SVC gate.** Change `encode_a32_svc(n)` (and the 4-byte stub case) to
  emit a branch into a gate table. Each table entry is `mov ip, #n`-style
  (or a literal load) and then `b common_gate`. `common_gate`:
  1. Stores all guest registers plus the resume PC (site + 4, the same as
     dynarmic's SVC semantics) into the context.
  2. Restores the host frame.
  3. Returns `CpuState::Svc(n)`.
- Constraint: 4-byte stub entries only fit one instruction, so the gate
  table must sit within ±32 MiB of `__symbol_stub` (allocate it right after
  the loaded image). The 8- and 12-byte sites can use `ldr pc,[pc,#-4]`
  with a literal. Clobbering `ip` (r12) at a call boundary is legal on
  Darwin.
- `swap_context` copies the saved register sets. No guest code is running
  at that point.
- `invalidate_cache_range` → `__builtin___clear_cache` (the ARM Linux
  `cacheflush` syscall). Needed after every stub rewrite.

### 3.3 Faults

- A `SIGSEGV/SIGBUS/SIGILL/SIGTRAP` handler runs on a `sigaltstack`. If the
  thread is in guest code (a flag set and cleared by enter/gate):
  1. Copy the `ucontext` registers into the guest context.
  2. Point the `ucontext` PC at the host-exit path.
  3. Return `Error(MemoryError | UndefinedInstruction | Breakpoint)`. The
     trap word `0xe7ffdefe` and `bkpt` map to these.
- Raw Darwin syscalls in app code (`svc 0x80`, rare) would reach Linux with
  junk numbers. Install a seccomp filter: `instruction_pointer` inside the
  arena → `SECCOMP_RET_TRAP` → SIGSYS → report.

### 3.4 Preemption instead of ticks

- Use `timer_create(SIGEV_THREAD_ID)` with a 5–10 ms period.
  - If the signal lands in guest code, do the same `ucontext` exit trick and
    return `Normal` with the tick budget at 0. The scheduler then calls
    `yield_thread` as it does today.
  - If it lands in host code, set a flag and let the next gate honour it.
- Single-step (`remaining_ticks = None`, used by touchHLE's gdb stub) is
  not supported natively. Disable `gdb.rs` under `cpu-native` and debug
  with the real gdb/gdbserver on the device.

### 3.5 Things that are not a problem natively

- Thumb-1/ARM interworking.
- r7/r9 conventions: all registers are swapped on every transition.
- musl's TLS (TPIDRURO): the guest never touches it, since libSystem is HLE.

**Milestone:** touchHLE's TestApp (`tests/`) passes natively on the device
with the same results as the dynarmic build on the Mac.

## 4. Phase 3: app bring-up on the device

In order:
1. TestApp.
2. A simple UIKit app from touchHLE's compatibility list.
3. A 2D GLES game.
4. A 3D GLES game with audio.

Compare each against the Mac dynarmic build on the same app. If the
behaviour differs, the native backend is at fault.

Device-specific items:

- **FBO support in `libn72gles` (top risk).** EAGL apps render into
  renderbuffers, which touchHLE emulates with `GL_OES_framebuffer_object`.
  If n72-gles lacks it, either add it to n72-gles or alias the app's colour
  renderbuffer to the window surface in `gles1_native.rs`/`present.rs`.
- **PVRTC:** touchHLE decompresses PVRTC on the CPU. The MBX supports PVRTC
  in hardware, so pass compressed uploads through if n72-gles exposes
  `IMG_texture_compression_pvrtc`.
- **Orientation:** the panel is 320×480 portrait. Check touchHLE's
  landscape rotation against the KMS output so it doesn't cost an extra
  copy.
- **Input:** touch through SDL kmsdrm/evdev (Zephyr2), the accelerometer
  through SDL sensors (if the kernel driver exposes an IIO/evdev
  accelerometer), the Home button → touchHLE's home/exit.
- **Audio:** openal-soft on ALSA (CS42L58).
- **RAM:** 128 MB, with zram already on. Watch RSS. Apps of this era used
  20–60 MB on iOS.

## 5. Phase 4: performance

Measure before optimising: gate round-trip cycles, and the per-frame count
of `objc_msgSend` and GL calls.

- The expected hotspot is `objc_msgSend`, which is a host function in
  touchHLE. Every message send costs a gate round trip plus a Rust lookup.
  Fix: an assembly `objc_msgSend` in the guest arena with a per-class
  selector→IMP cache mirrored into guest memory. Hits on guest-implemented
  methods never leave guest code; misses and host methods go through the
  gate.
- Make the gate lighter: save only d8–d15 when the host side may have
  clobbered them (it can't know, so measure whether a full VFP save
  matters).
- GL: batch nothing yet. `gles1_native` is already close to a direct call.
  Check the present-path copies.

## 6. Phase 5: integration

- An `n72hle` apk in `n72-alpine` (Rust build step in the Docker image).
- Apps stored on the iOS data volume next to `linux.img`, or on the Linux
  root.
- An n72-ui launcher entry per app: `.app`/`.ipa` icon + name from
  `Info.plist`. Launching stops the n72-ui GPU session and restores it on
  exit.

## 7. Risks: evaluated on the device, 2026-11-12

Test programs are in the session scratchpad (`risk-tests/`, `corotest/`).
Alpine's `sdl2` and `openal-soft-libs` are now installed on the device.

1. **FBO in libn72gles: CONFIRMED GAP, now implemented (n72-gles ec2e835, host-tested only).**
   - On the device, `GL_EXTENSIONS` = `GL_OES_read_format
     GL_OES_point_size_array`, and `glGenFramebuffersOES` is NULL.
     `glCompressedTexImage2D` rejects every format.
   - What touchHLE needs from the host:
     - `Gen/Bind/DeleteFramebuffersOES`, `FramebufferRenderbufferOES`,
       `RenderbufferStorageOES`, `GetRenderbufferParameterivOES`.
     - `CopyTexImage2D` + a textured quad for presentation
       (`eagl.rs present_renderbuffer` → `present.rs`).
     - `ReadPixels`.
   - Why it's feasible: n72-gles hands the MBX a render target per frame
     (`be_target`: gpu address, w/h, stride, depth buffer; `fr.dst_gpu` in
     `mbx_end`), and the blend path already samples a linear ARGB8888
     target as a texture. So:
     - a colour renderbuffer = an MBX buffer + an optional depth buffer;
     - a texture attachment = a linear (non-twiddled) texture;
     - `CopyTexImage2D` from the bound target can become a GPU blit
       instead of today's CPU copy (`gl_texture.c:540`).
   - Shortcut: when the app's renderbuffer is the fullscreen EAGL layer,
     render straight into the window surface and skip the copy plus quad.
   - PVRTC: optional. touchHLE already decodes it on the CPU. The MBX
     format bits are known (0x20000 / 0x38000, GPU.md), so a hardware path
     can come later.
   - Stencil: none (accepted and ignored). Apps that rely on it will draw
     wrongly.
2. **corosensei on ARMv6: CLEARED.** It has explicit Thumb-1/ARMv6 paths
   (build-time probes for r7/r9 and thumb2). Built for
   `arm-unknown-linux-musleabihf` and run on the iPod: resume/yield
   correct, panic unwinding across the coroutine caught, 200 000 round
   trips at **107 ns each**.
3. **SDL: CLEARED, plus one finding.**
   - The rust-sdl2 fork (`touchHLE-3`) binds SDL **2.26.4**. Alpine 3.24
     has real SDL2 **2.32.10** (ABI-compatible) and openal-soft 1.25.2.
     Use them; the `static` feature goes off.
   - On the device, SDL2 2.32.10 with kmsdrm:
     - GLES 1.1 context on `PowerVR MBX Lite (n72-gles TA backend)`;
     - 320×480 drawable with 24-bit depth;
     - 2 touch devices;
     - 120 clear+swap frames in 2.10 s (vsync).
   - Harmless "Could not restore CRTC" at quit; n72-ui came back fine.
   - Finding: `SDL_NumSensors() = 0`. SDL has no Linux accelerometer
     backend. The accelerometer is evdev `ST LIS3LV02DL Accelerometer`
     (event1), so the fork reads it directly in `window.rs`.
4. **Address layout: ONE KERNEL PATCH NEEDED.**
   - **ARM Linux refuses mappings below 0x2000.**
     `FIRST_USER_ADDRESS = PAGE_SIZE*2` (arch/arm/include/asm/pgtable.h:70),
     and `mmap(0x1000, MAP_FIXED)` gives EINVAL. Every iOS 4.2.1 app on this
     iPod has `__TEXT` at 0x1000, non-PIE; 33 binaries checked.
     DemoApp, Stocks, TrustMe, iOS Diagnostics and iPodOut start `__text`
     below 0x2000 (0x1d84–0x1f78), and the Mach-O header lives at 0x1000.
     The limit exists for low-vector CPUs (the vectors in page 0). This
     kernel uses high vectors (`[vectors]` at 0xffff0000, so
     `pgd_alloc`'s low-vector branch is skipped). Patch: `FIRST_USER_ADDRESS
     = PAGE_SIZE` for the N72 kernel. Needs a test that the zero page stays
     unmapped (null pointer dereferences must still fault).
   - **The host must not be a default PIE.** ARM's `ELF_ET_DYN_BASE` is
     0x400000, so a PIE loads at 4 MiB with its brk heap right after it,
     inside the guest image range. Link `n72hle` as a **dynamic non-PIE at
     0x70000000** (`-no-pie -Wl,--image-base=0x70000000`; for Rust,
     `-C relocation-model=static` + the same link argument). Tested on the
     device:
     - arena `0x2000–0x60000000` RWX `MAP_NORESERVE|MAP_FIXED_NOREPLACE`
       reserved (1.5 GiB);
     - A32 and Thumb code run from it;
     - `dlopen(libSDL2)` works;
     - host mmaps land at 0xb2…–0xb6…, the heap after 0x7003….
   - Other facts: 3G/1G split (stack at 0xbe…), vm.mmap_min_addr = 4096,
     overcommit 0, 113 MB RAM with 78 MB available while idle.
5. **Gate placement for 4-byte stubs: LOW RISK.** The 8-byte sites
   (`create_guest_function`, return-to-host) use `ldr pc,[pc,#-4]` + a
   literal, so their range doesn't matter. The 4-byte sites live in the
   app's `__symbol_stub` near 0x1000–a few MiB. A gate table allocated
   right after the image is always within ±32 MiB.
6. Merging upstream touchHLE changes later. Keep the native backend
   isolated behind the `Cpu` API to make rebases cheap.

Also seen: armv6 + armv7 fat binaries exist (Activator, Terminal). Pick the
armv6 slice (cpusubtype 6); the CPU can't run armv7.

## 8. First concrete steps

1. Phase 0 build on the device, plus `/proc/maps` of the running binary to
   pick `ARENA_END`.
2. Check `libn72gles` for `GL_OES_framebuffer_object` and PVRTC
   (`glGetString(GL_EXTENSIONS)`).
3. A standalone assembly prototype of enter/gate/fault-exit with a
   hand-written armv6 blob, before wiring it into touchHLE.
