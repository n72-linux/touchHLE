#!/bin/sh
# ab-prof.sh RUNDIR KIT NAME [LINES]: the N72_PROF profile of one gofab.sh run (PROF=1) as a table.
# RUNDIR is a fetched run (ab-results/<dir>/runs/<route>-<round>-<name>, holds prof.bin and
# maps.txt); KIT and NAME find the variant's Mac files in ab-kits/KIT.sources.txt, the ones
# with symbols. A variant that runs the installed touchHLE or libn72gles needs the Mac file
# of that build given: HLE=path LIB=path (libn72gles.so.1.syms) in the environment; a rebuild
# of the same commit is not the same binary (paths, compiler), so its touchHLE names are only
# a guess: for profiles put touchHLE in the kit from an ab-build.sh output.
# musl: the device's copy in the results (PROF=1 runs), else the sysroot's (may differ).
set -e
W=$(cd "$(dirname "$0")/../../.." && pwd)
T=$(cd "$(dirname "$0")" && pwd)
d=$1; kit=$2; name=$3; lines=${4:-45}
[ -f "$d/prof.bin" ] && [ -f "$d/maps.txt" ] || { sed -n '2,8p' "$0"; exit 2; }
. $W/n72-alpine/toolchain/env.sh
set -- $(awk -v n="$name" '$1 == n {print $2, $3}' $W/ab-kits/$kit.sources.txt)
mh=${HLE:-$1}; ml=${LIB:-$2}
[ -f "$mh" ] || { echo "touchHLE with symbols: give HLE=path ($1)"; exit 1; }
[ -f "$ml" ] || { echo "libn72gles with symbols: give LIB=path ($2)"; exit 1; }
${CROSS}nm -n -C --defined-only "$mh" | grep -i ' [tw] ' > $d/hle.syms
${CROSS}nm -n --defined-only "$ml" | grep -i ' [tw] ' > $d/gles.syms
m=$d/../../ld-musl-armhf.so.1	# the device's, saved by gofab.sh PROF=1
[ -f $m ] || m=$W/alpine-work/sysroot-armhf/lib/ld-musl-armhf.so.1
${CROSS}nm -n -D --defined-only $m | grep -i ' [tw] ' > $d/musl.syms
echo "musl on the Mac: $(md5 -q $m | cut -c1-8); on the device: $(grep -h '^musl' $d/../../info.txt 2>/dev/null)"
cp $W/ab-kits/syms/gof.syms $d/
python3 -I $T/prof-report.py $d $lines
