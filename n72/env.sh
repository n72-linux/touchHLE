# Source this to cross-build the n72 native backend for the iPod touch 2G:
#   . n72/env.sh && cargo build --release --target arm-unknown-linux-musleabihf \
#       --no-default-features --features cpu-native
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
