#!/bin/sh
# gofab.sh ROUTE ROUNDS VARIANTS [NAME...]: Galaxy on Fire A/B runs on the iPod.
#
# VARIANTS is a file of lines "name binary libdir [VAR=value ...]" (# comments):
#   binary  a touchHLE binary (relative to /root/n72hle-test, or absolute)
#   libdir  a directory with libn72gles.so.1 and its libEGL/libGLESv1_CM/libgbm
#           links, or - for the installed /usr/lib one
# NAMEs pick some of the variants (default all). Every round runs each variant once, the order
# rotated by one each round (ABC, BCA, CAB) so that warm-up and battery drift do not favour one.
#
# ROUTE (seconds from the start, taps through n72-tapd; window = what is measured):
#   menu    Audio On at 20 s; window 30-50 s, the main menu
#   modes   + Start Game at 45 s; window 50-57 s, the mode screen
#   flight  + Survival Mode, Start x2; window 100-130 s, survival flight with enemies
# Every run: N72_FIXED_SEED=1, n72-ui stopped, panel on, backlight 0 (restored at the end).
# PROF=1: also the N72_PROF sampler over the window (prof.bin + maps.txt, for prof-report.py);
# the sampler rides on the existing time-slice tick, but keep timing runs and profile runs apart.
#
# Per run, in OUT/runs/<route>-<round>-<name>/: log, cpu.txt (thread CPU over the window),
# gpu.txt. One line per run in OUT/results.tsv:
#   route round name fps_med fps_min fps_max seconds main_ms_frame other_ms_frame ta_us render_us
# main_ms_frame: the busiest thread's CPU time per frame (the game's), other: the rest together.
# OUT is /root/gof/ab/<date>-<route> (or $OUT). Stops at once on a kernel warning or a crash.
R=$1; N=$2; V=$3
[ -n "$R" ] && [ -n "$N" ] && [ -f "$V" ] || { sed -n '2,24p' "$0"; exit 2; }
shift 3
H=/root/n72hle-test
APP=touchHLE_apps/GalaxyOnFire.app
case $R in
menu)   TAPS="20:156,240"; W0=30; W1=50 ;;
modes)  TAPS="20:156,240 45:118,240"; W0=50; W1=57 ;;
flight) TAPS="20:156,240 45:118,240 58:194,240 72:305,72 84:305,72"; W0=100; W1=130 ;;
*) echo "route: menu, modes or flight"; exit 2 ;;
esac
OUT=${OUT:-/root/gof/ab/$(date +%Y%m%d-%H%M)-$R}
mkdir -p $OUT/runs || exit 1
STATS=/sys/kernel/debug/apple-mbx/stats
LCD=/sys/devices/platform/soc/38900000.lcd/panel_power

busy() {
	for p in /proc/[0-9]*; do
		e=$(readlink $p/exe 2>/dev/null)
		case "${e##*/}" in touchHLE*|retroarch|n72-player|netsurf*) echo "${e##*/} ($p)"; return 0;; esac
	done
	return 1
}
restore() {
	[ -f /root/gof/brightness.saved ] && for f in /sys/class/backlight/*/brightness; do cat /root/gof/brightness.saved > $f; done
	rm -f /root/gof/brightness.saved
	tp=$(pidof n72-tapd) && kill $tp
	pidof n72-ui >/dev/null || rc-service n72-ui start >/dev/null
}
# CPU time (utime + stime, clock ticks) of every thread of $1: "tid comm ticks" lines
threads() {
	for t in /proc/$1/task/*; do
		read -r s < $t/stat 2>/dev/null || continue
		c=$(cat $t/comm 2>/dev/null)
		set -- ${s##*) }
		echo "${t##*/} ${c:-?} $((${12} + ${13}))"
	done
}

b=$(busy) && { echo "busy: $b"; exit 1; }
# the variants
NV=0
while read -r name bin lib env; do
	case $name in ''|'#'*) continue ;; esac
	if [ $# -gt 0 ]; then
		ok=; for w; do [ "$w" = "$name" ] && ok=1; done; [ -n "$ok" ] || continue
	fi
	case $bin in /*) ;; *) bin=$H/$bin ;; esac
	[ -x "$bin" ] || { echo "$name: no $bin"; exit 1; }
	[ "$lib" = - ] || [ -f "$lib/libn72gles.so.1" ] || { echo "$name: no $lib/libn72gles.so.1"; exit 1; }
	NV=$((NV + 1))
	eval "v_name_$NV=\$name v_bin_$NV=\$bin v_lib_$NV=\$lib v_env_$NV=\$env"
	l=$lib; [ "$l" = - ] && l=/usr/lib
	echo "$name $(md5sum < $bin | cut -c1-8) lib $(md5sum < $l/libn72gles.so.1 | cut -c1-8) env $env" >> $OUT/variants.txt
done < $V
[ $NV -gt 0 ] || { echo "no variants"; exit 1; }
[ -f $H/GalaxyOnFire.env ] && echo "note: $H/GalaxyOnFire.env exists, it is NOT read here (only the launcher reads it)"
{
	echo "route $R rounds $N window $W0-$W1 s, $(date)"
	echo "kernel $(uname -r) $(uname -v)"
	echo "musl $(md5sum < /lib/ld-musl-armhf.so.1 | cut -c1-8)"
	echo "battery $(cat /sys/class/power_supply/*/capacity 2>/dev/null | head -1) % $(cat /sys/class/power_supply/*/status 2>/dev/null | head -1)"
	cat $OUT/variants.txt
} > $OUT/info.txt
cat $OUT/info.txt
[ -f $OUT/results.tsv ] || echo "route	round	name	fps_med	fps_min	fps_max	seconds	main_ms_frame	other_ms_frame	ta_us	render_us" > $OUT/results.tsv

pidof n72-ui >/dev/null && { rc-service n72-ui stop >/dev/null; sleep 1; }
[ -f /root/gof/brightness.saved ] || cat /sys/class/backlight/*/brightness > /root/gof/brightness.saved
echo 1 > $LCD
for f in /sys/class/backlight/*/brightness; do echo 0 > $f; done
dmesg | wc -l > $OUT/dmesg.start
HZ=100	# USER_HZ: /proc/*/stat counts in it

r=1
while [ $r -le $N ]; do
	k=0
	while [ $k -lt $NV ]; do
		i=$(( (k + r - 1) % NV + 1 )); k=$((k + 1))
		eval "name=\$v_name_$i bin=\$v_bin_$i lib=\$v_lib_$i env=\$v_env_$i"
		d=$OUT/runs/$R-$r-$name; rm -rf $d; mkdir -p $d
		tp=$(pidof n72-tapd) && { kill $tp; sleep 1; }	# a fresh one: an old one drops taps
		setsid /usr/local/bin/n72-tapd /tmp/n72-tap > /tmp/tapd.log 2>&1 < /dev/null &
		sleep 1
		pe=; [ -n "$PROF" ] && pe="N72_PROF=$d/prof.bin N72_PROF_SKIP=$W0"
		le=; [ "$lib" = - ] || le="LD_LIBRARY_PATH=$lib"
		cd $H
		env $env $le $pe N72_FIXED_SEED=1 N72_GLES_VGP=1 SDL_VIDEODRIVER=kmsdrm RUST_BACKTRACE=1 \
			$bin $APP --print-fps > $d/log 2>&1 &
		p=$!
		now=0
		for t in $TAPS $W0:win; do
			at=${t%%:*}; xy=${t#*:}
			[ $at -gt $now ] && sleep $((at - now)); now=$at
			[ "$xy" = win ] && break
			echo "hold ${xy%,*} ${xy#*,} 150" > /tmp/n72-tap
			echo "tap $xy at $at s" >> $d/taps
		done
		kill -0 $p 2>/dev/null || { echo "$name: touchHLE ended before the window"; tail -5 $d/log; restore; exit 1; }
		f0=$(grep -c ' FPS: ' $d/log); cat $STATS > /dev/null 2>&1; threads $p > $d/cpu0
		sleep $((W1 - W0))
		f1=$(grep -c ' FPS: ' $d/log); g=$(grep 'queued frames' $STATS 2>/dev/null); threads $p > $d/cpu1
		[ -n "$PROF" ] && { kill -USR2 $p; sleep 1; cat /proc/$p/maps > $d/maps.txt; }
		kill -9 $p 2>/dev/null; wait $p 2>/dev/null
		echo "$g" > $d/gpu.txt
		grep ' FPS: ' $d/log | sed -n "$((f0 + 1)),${f1}p" | awk '{print $NF}' | sort -n > $d/fps.txt
		frames=$(awk '{s += $1} END {printf "%d", s}' $d/fps.txt)
		# thread CPU over the window, per frame
		awk -v hz=$HZ -v fr=$frames 'NR == FNR {a[$1] = $3; next} {d = $3 - a[$1]; if (d > 0) printf "%s %s %.1f\n", $1, $2, (fr ? d * 1000 / hz / fr : 0)}' \
			$d/cpu0 $d/cpu1 | sort -k3 -rn > $d/cpu.txt
		main=$(awk 'NR == 1 {print $3}' $d/cpu.txt); other=$(awk 'NR > 1 {s += $3} END {printf "%.1f", s}' $d/cpu.txt)
		ta=$(echo "$g" | sed -n 's/.*TA \([0-9]*\) us.*/\1/p'); rd=$(echo "$g" | sed -n 's/.*render \([0-9]*\) us.*/\1/p')
		line=$(awk -v r=$R -v n=$r -v nm=$name -v m=${main:-0} -v o=${other:-0} -v ta=${ta:-0} -v rd=${rd:-0} \
			'{v[NR] = $1} END {if (!NR) {printf "%s\t%s\t%s\t0\t0\t0\t0\t%s\t%s\t%s\t%s", r, n, nm, m, o, ta, rd; exit}
			 printf "%s\t%s\t%s\t%s\t%s\t%s\t%d\t%s\t%s\t%s\t%s", r, n, nm, v[int((NR + 1) / 2)], v[1], v[NR], NR, m, o, ta, rd}' $d/fps.txt)
		echo "$line" >> $OUT/results.tsv
		echo "$line"
		# warning signs: a crash in the log or new kernel warnings stop everything
		if grep -qE 'panicked|assertion|Segmentation|SIGSEGV|Illegal' $d/log; then
			echo "$name: the log shows a crash, stopping"; grep -m3 -E 'panicked|assertion' $d/log; restore; exit 1
		fi
		if dmesg | tail -n +$(( $(cat $OUT/dmesg.start) + 1 )) | grep -qiE 'oops|bug:|warning:|hung|timed out|timeout'; then
			echo "kernel warning, stopping:"; dmesg | tail -n +$(( $(cat $OUT/dmesg.start) + 1 )) | tail -5; restore; exit 1
		fi
		sleep 2
	done
	r=$((r + 1))
done
restore
echo "results: $OUT/results.tsv"
