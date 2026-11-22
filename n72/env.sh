# Source this to cross-build the n72 native backend for the iPod touch 2G:
#   . n72/env.sh && cargo build --release --target arm-unknown-linux-musleabihf \
#       --no-default-features --features cpu-native,openal-static
# (openal-static: OpenAL Soft from the vendor/openal-soft submodule, built for
# the ARM1176 and linked in; n72/alsoft.conf goes next to the binary.)
# Needs rustup's stable toolchain with the arm-unknown-linux-musleabihf target
# and the exported Alpine armhf sysroot. rustc is LLVM whatever is chosen
# here; N72_CC picks who compiles the C and C++ (OpenAL Soft) and links:
#   gcc (default)  the cross gcc of ../n72-alpine/toolchain (GNU ld; its own
#                  libstdc++ linked statically)
#   clang          Homebrew clang + lld (n72/armhf-cc); n72/pgo/build.sh's
#                  profiled and LTO variants use this one
N72_HLE=$(cd "$(dirname "${BASH_SOURCE:-${(%):-%x}}")/.." && pwd)
N72_SYSROOT=${N72_SYSROOT:-$N72_HLE/../alpine-work/sysroot-armhf}
export N72_SYSROOT
if [ "${N72_CC:-gcc}" = gcc ]; then
	N72_ENV=$N72_HLE/../n72-alpine/toolchain/env.sh
	. "$N72_ENV"
fi
# rustup's toolchain first (Homebrew has a rustc too, without this target)
export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"
if [ "${N72_CC:-gcc}" = gcc ]; then
	export CC_arm_unknown_linux_musleabihf="$N72_TC/bin/armv6-alpine-linux-musleabihf-gcc"
	export CXX_arm_unknown_linux_musleabihf="$N72_TC/bin/armv6-alpine-linux-musleabihf-g++"
	export AR_arm_unknown_linux_musleabihf="$N72_TC/bin/armv6-alpine-linux-musleabihf-gcc-ar"
	export N72_TC
	export CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_LINKER="$N72_HLE/n72/link-gcc"
else
	export CC_arm_unknown_linux_musleabihf="$N72_HLE/n72/armhf-cc"
	export CXX_arm_unknown_linux_musleabihf="$N72_HLE/n72/armhf-c++"
	export AR_arm_unknown_linux_musleabihf=/opt/homebrew/opt/llvm/bin/llvm-ar
	export CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_LINKER="$N72_HLE/n72/armhf-cc"
fi
# Dynamic (SDL2, OpenAL and GLES come from Alpine and n72-gles), not PIE, and
# linked at 0x70000000 so the guest arena 0x1000-0x60000000 stays free (ARM
# Linux loads PIEs at 4 MiB).
export CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS="-C target-feature=-crt-static -C relocation-model=static -C link-arg=-no-pie -C link-arg=-Wl,--image-base=0x70000000"
# clang: the sysroot's libraries by an explicit -L. Not for gcc: it finds its
# sysroot itself, and a -L ahead of its own directories would put the
# sysroot's libstdc++.so (gcc 15.2's) before the libstdc++.a that belongs to
# the headers OpenAL Soft was compiled with.
[ "${N72_CC:-gcc}" = gcc ] ||
	CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS="$CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS -L $N72_SYSROOT/usr/lib"
# -O2, the norm (n72-alpine/BUILD-FLAGS.md): Spore and altest measured no
# different at -O3 (n72/pgo/README). OpenAL Soft follows the profile's level.
export CARGO_PROFILE_RELEASE_OPT_LEVEL=2
