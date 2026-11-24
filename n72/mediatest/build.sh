#!/bin/sh
# Build N72MediaTest.app with the ios-armv6-tc Docker image (no iOS SDK:
# decls.h and stubs/*.tbd stand in for the headers and frameworks).
#   sh build.sh [OUT_DIR]
# The .app lands in OUT_DIR (default: $N72_MEDIATEST_OUT, else
# ${TMPDIR:-/tmp}/n72-mediatest-out; never inside the repository).
# Docker on this Mac does not see /private/tmp, so the build runs inside
# the container and the .app comes back as a tar stream.
# Compiled and linked for iPhone OS 3.0: classic external relocations and
# indirect symbols, no LC_DYLD_INFO. Do not link for 3.1+: ld64 then emits
# compressed binds whose backward address steps are 64-bit ULEBs, which
# the mach_object crate (0.1.17, read_uleb128 into usize) misreads on the
# 32-bit device, so later binds (the NSObject metaclass) land elsewhere.
# start.s gives the LC_UNIXTHREAD start and the dyld_stub_binding_helper
# that crt1.o would (touchHLE rewrites the stubs; the helper never runs).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-${N72_MEDIATEST_OUT:-${TMPDIR:-/tmp}/n72-mediatest-out}}
app=N72MediaTest.app
mkdir -p "$out"
rm -rf "$out/$app"

docker run --rm -i -v "$here":/src:ro ios-armv6-tc sh -euc '
  exec 3>&1 1>&2         # stdout carries the tar only
  T="-target armv6-apple-ios3.0"
  mkdir -p /w/obj /w/'$app'
  cd /w
  clang $T -c /src/start.s -o obj/start.o
  clang $T -O1 -Wall -fno-objc-arc -fno-objc-exceptions -fno-stack-protector \
    -c /src/main.m -o obj/main.o
  arm-apple-darwin11-ld -arch armv6 -ios_version_min '"${N72_LD_IOS_MIN:-3.0}"' -no_pie -Z \
    -o '$app'/N72MediaTest obj/start.o obj/main.o \
    /src/stubs/libSystem.tbd /src/stubs/libobjc.tbd \
    /src/stubs/CoreFoundation.tbd /src/stubs/Foundation.tbd \
    /src/stubs/UIKit.tbd /src/stubs/MediaPlayer.tbd \
    /src/stubs/AVFoundation.tbd
  ldid -S '$app'/N72MediaTest
  cp /src/Info.plist /src/test.txt '$app'/
  printf "APPL????" > '$app'/PkgInfo
  tar -cf - '$app' >&3
' > "$out/.app.tar" || { rm -f "$out/.app.tar"; echo "build failed" >&2; exit 1; }
tar -xf "$out/.app.tar" -C "$out"
rm -f "$out/.app.tar"

echo "built $out/$app:"
ls -l "$out/$app"
