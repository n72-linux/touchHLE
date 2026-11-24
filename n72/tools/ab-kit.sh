#!/bin/sh
# ab-kit.sh KIT NAME=HLE,LIB[,VAR=value...] ...: a kit of variants for gofab.sh, packed for the iPod.
#   HLE  installed (/root/n72hle-test/touchHLE as it is on the device), a device path (/...
#        on the iPod, e.g. /root/n72hle-test/touchHLE.c1c8834), or a Mac file (an ab-build.sh output;
#        use one for PROF=1 runs: only then are the profile's touchHLE names certain)
#   LIB  installed (/usr/lib), or a Mac directory or libn72gles.so.1 (an ab-build.sh output)
# Example:
#   ab-kit.sh routing base=installed,installed new=installed,ab-kits/builds/lib-d6850c8
# Writes ~/n72-linux/ab-kits/KIT/ (variants.txt for the device, sources.txt with the Mac
# files that keep the symbols, stripped copies) and ab-kits/KIT.tar.gz (root-owned, made by
# roottar.py). On the iPod: tar xzf KIT.tar.gz -C /root  ->  /root/ab/KIT, then
#   sh /root/ab/KIT/gofab.sh menu 3 /root/ab/KIT/variants.txt
set -e
W=$(cd "$(dirname "$0")/../../.." && pwd)
T=$(cd "$(dirname "$0")" && pwd)
kit=$1; shift
[ -n "$kit" ] && [ $# -gt 0 ] || { sed -n '2,12p' "$0"; exit 2; }
. $W/n72-alpine/toolchain/env.sh
S=$W/ab-kits/.stage; D=$S/ab/$kit; DEV=/root/ab/$kit
rm -rf $S; mkdir -p $D
: > $D/variants.txt; : > $W/ab-kits/$kit.sources.txt
for spec; do
	name=${spec%%=*}; rest=${spec#*=}
	hle=${rest%%,*}; rest=${rest#*,}
	lib=${rest%%,*}; env=; [ "$rest" = "$lib" ] || env=$(echo "${rest#*,}" | tr ',' ' ')
	case $hle in
	installed) dh=/root/n72hle-test/touchHLE; mh=installed ;;
	/root/*|/usr/*) dh=$hle; mh=device ;;
	*) f=$hle; [ -f "$f" ] || f=$W/$hle; [ -d "$f" ] && f=$f/touchHLE; [ -f "$f" ] || { echo "$name: no $hle"; exit 1; }
	   mh=$(cd "$(dirname "$f")" && pwd)/touchHLE
	   dh=$(awk -v m="$mh" '$2 == m {print $3; exit}' $S/hle.list 2>/dev/null)	# one copy per binary
	   if [ -z "$dh" ]; then
		mkdir -p $D/$name; ${CROSS}strip -o $D/$name/touchHLE "$f"; dh=$DEV/$name/touchHLE
		echo "$name $mh $dh" >> $S/hle.list
	   fi ;;
	esac
	case $lib in
	installed) dl=-; ml=installed ;;
	*) f=$lib; [ -e "$f" ] || f=$W/$lib; [ -d "$f" ] && f=$f/libn72gles.so.1; [ -f "$f" ] || { echo "$name: no $lib"; exit 1; }
	   mkdir -p $D/$name/lib; cp "$f" $D/$name/lib/libn72gles.so.1
	   for n in libEGL libGLESv1_CM libgbm; do ln -sf libn72gles.so.1 $D/$name/lib/$n.so.1; done
	   dl=$DEV/$name/lib; ml=$(cd "$(dirname "$f")" && pwd)/libn72gles.so.1; [ -f "$ml.syms" ] && ml=$ml.syms ;;
	esac
	echo "$name $dh $dl $env" >> $D/variants.txt
	echo "$name $mh $ml" >> $W/ab-kits/$kit.sources.txt
done
cp $T/gofab.sh $D/
rm -f $S/hle.list
python3 -I $W/n72-alpine/toolchain/roottar.py $W/ab-kits/$kit.tar.gz $S ab
rm -rf $W/ab-kits/$kit; mv $D $W/ab-kits/$kit; rm -rf $S
echo "kit $W/ab-kits/$kit.tar.gz ($(du -k $W/ab-kits/$kit.tar.gz | cut -f1) KB):"
cat $W/ab-kits/$kit/variants.txt
