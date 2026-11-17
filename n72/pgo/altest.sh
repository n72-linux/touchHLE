#!/bin/sh
# altest.sh VARIANT...: n72/pgo/altest.c linked against each variant's OpenAL Soft (as built for its
# touchHLE, link-time optimised where the variant is), static, in target/n72-var/VARIANT/altest
H=$(cd "$(dirname "$0")/../.." && pwd)
for V in "$@"; do
	L=$(ls -d "$H"/target/n72-var/$V/arm-unknown-linux-musleabihf/release/build/touchHLE_openal_soft_wrapper-*/out/linux/lib | head -1)
	case $V in o3lto) F="-flto -Wl,--lto-O3" ;; *lto) F="-flto -Wl,--lto-O2" ;; *) F= ;; esac
	case $V in gen) F="-fprofile-generate -resource-dir=$H/target/n72-var/resource-dir" ;; esac
	"$H/n72/armhf-c++" -O2 -static $F -I"$H/vendor/openal-soft/include" -x c "$H/n72/pgo/altest.c" -x none \
		"$L/libopenal.a" -lm -lpthread -o "$H/target/n72-var/$V/altest" && echo "$V: $(md5 -q "$H/target/n72-var/$V/altest")"
done
