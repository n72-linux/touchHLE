#!/bin/sh
# ab-build.sh lib COMMIT [gcc|clang]   n72-gles at COMMIT -> ~/n72-linux/ab-kits/builds/lib-<commit>[-clang]/
# ab-build.sh hle COMMIT [gcc|clang]   touchHLE at COMMIT  -> ~/n72-linux/ab-kits/builds/hle-<commit>[-clang]/touchHLE
# On the Mac, from the commit alone (git archive / a private copy of the touchHLE tree), so the
# checkouts other sessions work in are never touched. The outputs keep their symbols (the
# kits strip what goes to the device); a build that exists is not made again.
# touchHLE is built in ~/n72-linux/touchHLE-ab (an rsync copy of touchHLE without target/,
# checked out at COMMIT; its target/ is kept between builds). Its submodules come along as they
# are in the touchHLE checkout: a COMMIT that needs other submodule commits is refused.
set -e
W=$(cd "$(dirname "$0")/../../.." && pwd)
K=$W/ab-kits/builds
what=$1; c=$2; cc=${3:-gcc}
[ -n "$c" ] || { sed -n '2,10p' "$0"; exit 2; }
case $cc in gcc) sfx= ;; clang) sfx=-clang ;; *) echo "compiler: gcc or clang"; exit 2 ;; esac
case $what in
lib)
	h=$(git -C $W/n72-gles rev-parse --short "$c")
	o=$K/lib-$h$sfx
	[ -f $o/libn72gles.so.1 ] && { echo "$o (exists)"; exit 0; }
	s=$K/src-lib-$h; rm -rf $s $o; mkdir -p $s $o
	git -C $W/n72-gles archive $h | tar -x -C $s
	. $W/n72-alpine/toolchain/env.sh
	# an old commit's Makefile may predate the Mac toolchain: then the newest Makefile that
	# knows N72_CC and names exactly the source files this commit has
	if ! grep -q 'N72_CC' $s/Makefile; then
		for m in $(git -C $W/n72-gles log --format=%h main -- Makefile); do
			git -C $W/n72-gles show $m:Makefile > $s/Makefile.try
			grep -q 'N72_CC' $s/Makefile.try || continue
			ok=1
			for f in $(grep -o 'src/[a-z_0-9]*\.[cS]' $s/Makefile.try | sort -u); do [ -f $s/$f ] || ok=; done
			for f in $(cd $s && ls src/*.c src/*.S); do grep -q "$f" $s/Makefile.try || case $f in src/be_sw*|src/*host*) ;; *) ok=;; esac; done
			[ -n "$ok" ] && { mv $s/Makefile.try $s/Makefile; echo "(Makefile of $m)"; break; }
		done
		grep -q 'N72_CC' $s/Makefile || { echo "no Makefile fits $h"; exit 1; }
	fi
	(cd $s && gmake -s -j8 N72_CC=$cc B=$o/b BIN=$o/bin WORK=$W $o/b/libn72gles.so.1)
	cp $o/b/libn72gles.so.1 $o/b/libn72gles.so.1.syms $o/
	rm -rf $o/b $o/bin $s
	for n in libEGL libGLESv1_CM libgbm; do ln -sf libn72gles.so.1 $o/$n.so.1; done
	echo "n72-gles $h $cc $(cd $o && md5 -q libn72gles.so.1)" > $o/SOURCE
	cat $o/SOURCE ;;
hle)
	h=$(git -C $W/touchHLE rev-parse --short "$c")
	o=$K/hle-$h$sfx
	[ -f $o/touchHLE ] && { echo "$o (exists)"; exit 0; }
	A=$W/touchHLE-ab
	rsync -a --delete --exclude /target --exclude /touchHLE_sandbox --exclude /touchHLE_apps $W/touchHLE/ $A/
	git -C $A checkout -q --detach -f $h
	git -C $A submodule status | grep -q '^+' && { echo "$h needs other submodule commits than the touchHLE checkout has"; git -C $A submodule status; exit 1; }
	(cd $A && export N72_CC=$cc && . n72/env.sh && cargo build -q --release --target arm-unknown-linux-musleabihf \
		--no-default-features --features cpu-native,openal-static --target-dir target/$cc)
	mkdir -p $o
	cp $A/target/$cc/arm-unknown-linux-musleabihf/release/touchHLE $o/
	echo "touchHLE $h $cc $(md5 -q $o/touchHLE)" > $o/SOURCE
	cat $o/SOURCE ;;
*) sed -n '2,10p' "$0"; exit 2 ;;
esac
