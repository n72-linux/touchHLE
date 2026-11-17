#!/bin/bash
# n72/pgo/build.sh VARIANT: the n72 binary built one of several ways, each in its own target
# directory (target/n72-var/VARIANT/.../touchHLE); see n72/pgo/README.
#   base     -O3, no LTO, no profile (as the installed builds so far)
#   gen      instrumented for a profile (Rust and OpenAL Soft), -O2
#   o3pgo    -O3 with n72/pgo/touchHLE.profdata
#   o2lto    -O2, LTO, profile        o3lto    -O3, LTO, profile
# LTO here: Rust crates as one unit (lto=fat, one codegen unit) and OpenAL Soft's objects optimised
# together at link time.
set -e
V=${1:?variant}
H=$(cd "$(dirname "$0")/../.." && pwd)
. "$H/n72/env.sh"
RT=$H/../alpine-work/src/compiler-rt-23/build/libclang_rt.profile-armhf.a
PROF=$H/n72/pgo/touchHLE.profdata
T=arm-unknown-linux-musleabihf
R=$CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS
export CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_LINKER=$H/n72/pgo/link-cc
CF=; LTO=false; CGU=16; OPT=3
NOWARN="-Wno-profile-instr-unprofiled -Wno-profile-instr-out-of-date -Wno-backend-plugin"
case $V in
base) ;;
gen)	OPT=2
	export RUSTC_BOOTSTRAP=1	# -Z no-profiler-runtime: rustup ships no profiler_builtins for this target
	R="$R -C profile-generate=/root/n72hle-test/pgo -Z no-profiler-runtime --cfg n72_pgo_gen -C link-arg=-Wl,-u,__llvm_profile_runtime -C link-arg=$RT"
	# clang links its profile runtime from its resource directory, which has none for this target
	# (CMake's compiler checks link): a copy of that directory with ours in it
	RD=$H/target/n72-var/resource-dir; CRD=$(/opt/homebrew/opt/llvm/bin/clang -print-resource-dir)
	mkdir -p "$RD/lib/armv6-alpine-linux-musleabihf"; ln -sfn "$CRD/include" "$RD/include"
	cp "$RT" "$RD/lib/armv6-alpine-linux-musleabihf/libclang_rt.profile.a"
	CF="-fprofile-generate -resource-dir=$RD" ;;
o3pgo|o2lto|o3lto)
	[ -f "$PROF" ] || { echo "no $PROF" >&2; exit 1; }
	R="$R -C profile-use=$PROF -C llvm-args=-hot-cold-split=true"
	CF="-fprofile-use=$PROF -mllvm -hot-cold-split=true $NOWARN"
	case $V in o2lto) OPT=2 ;; esac
	case $V in *lto) LTO=fat; CGU=1; CF="$CF -flto"
		R="$R -C link-arg=-Wl,--lto-O$OPT -C link-arg=-Wl,-mllvm,-hot-cold-split=true -C link-arg=-Wl,-z,keep-text-section-prefix" ;; esac ;;
*) echo "unknown variant $V" >&2; exit 1 ;;
esac
export CARGO_TARGET_ARM_UNKNOWN_LINUX_MUSLEABIHF_RUSTFLAGS="$R"
export CFLAGS_arm_unknown_linux_musleabihf="$CF" CXXFLAGS_arm_unknown_linux_musleabihf="$CF"
export CARGO_PROFILE_RELEASE_OPT_LEVEL=$OPT CARGO_PROFILE_RELEASE_LTO=$LTO CARGO_PROFILE_RELEASE_CODEGEN_UNITS=$CGU
export CARGO_TARGET_DIR=$H/target/n72-var/$V
cd "$H"
cargo build --release --target $T --no-default-features --features cpu-native,openal-static
B=$CARGO_TARGET_DIR/$T/release/touchHLE
echo "$V: $(md5 -q "$B") $(stat -f %z "$B") bytes"
