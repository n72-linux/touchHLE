# Source this to cross-build the n72 native backend for the iPod touch 2G:
#   . n72/env.sh && cargo build --release --target arm-unknown-linux-musleabihf \
#       --no-default-features --features cpu-native,openal-static
# (openal-static: OpenAL Soft from the vendor/openal-soft submodule, built for
# the ARM1176 and linked in; n72/alsoft.conf goes next to the binary.)
# Needs rustup's stable toolchain with the arm-unknown-linux-musleabihf target,
# Homebrew LLVM (clang, llvm-ar) and the exported Alpine armhf sysroot.
N72_HLE=$(cd "$(dirname "${BASH_SOURCE:-${(%):-%x}}")/.." && pwd)
N72_SYSROOT=${N72_SYSROOT:-$N72_HLE/../alpine-work/sysroot-armhf}
export N72_SYSROOT
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
export CC_arm_unknown_linux_musleabihf="$N72_HLE/n72/armhf-cc"
export CXX_arm_unknown_linux_musleabihf="$N72_HLE/n72/armhf-c++"
export AR_arm_unknown_linux_musleabihf=/opt/homebrew/opt/llvm/bin/llvm-ar
export CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_LINKER="$N72_HLE/n72/armhf-cc"
# Dynamic (SDL2, OpenAL and GLES come from Alpine and n72-gles), not PIE, and
# linked at 0x70000000 so the guest arena 0x1000-0x60000000 stays free (ARM
# Linux loads PIEs at 4 MiB).
export CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS="-C target-feature=-crt-static -C relocation-model=static -C link-arg=-no-pie -C link-arg=-Wl,--image-base=0x70000000 -L $N72_SYSROOT/usr/lib"
# -O2, the norm (n72-alpine/BUILD-FLAGS.md): Spore and altest measured no different at -O3 (2026-11-21,
# n72/pgo/README). OpenAL Soft follows the profile's level (its build.rs).
export CARGO_PROFILE_RELEASE_OPT_LEVEL=2
