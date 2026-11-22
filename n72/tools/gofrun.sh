#!/bin/sh
# gofrun.sh BIN NAME SECS [VAR=value...]: Galaxy on Fire started as the launcher does (but with n72-ui
# stopped and the panel on), left SECS seconds, then a profile dump and kill. Results in /root/gof/NAME.
# TAPS="secs:x,y secs:x,y ..." (seconds after the start, panel pixels) are sent through n72-tapd.
# The caller restores the brightness and restarts n72-ui (gofend.sh).
b=$1; n=$2; t=$3; shift 3
cd /root/n72hle-test || exit 1
for p in /proc/[0-9]*; do
	case "$(cat $p/comm 2>/dev/null)" in
	touchHLE*|retroarch|n72-player|netsurf*) echo "busy: $p $(cat $p/comm)"; exit 1;;
	esac
done
pidof n72-ui >/dev/null && { rc-service n72-ui stop >/dev/null; sleep 1; }
[ -f /root/gof/brightness.saved ] || cat /sys/class/backlight/*/brightness > /root/gof/brightness.saved
echo 1 > /sys/devices/platform/soc/38900000.lcd/panel_power
for f in /sys/class/backlight/*/brightness; do echo 0 > $f; done
d=/root/gof/$n; rm -rf $d; mkdir -p $d
# a fresh n72-tapd per run: one left from the run before did not deliver its tap
tp=$(pidof n72-tapd) && { kill $tp; sleep 1; }
setsid /usr/local/bin/n72-tapd /tmp/n72-tap >/tmp/tapd.log 2>&1 </dev/null &
sleep 1
taps=$TAPS; unset TAPS
cat /sys/kernel/debug/apple-mbx/stats > /dev/null 2>&1
env "$@" N72_PROF=$d/prof.bin SDL_VIDEODRIVER=kmsdrm RUST_BACKTRACE=1 $b touchHLE_apps/GalaxyOnFire.app --print-fps > $d/log 2>&1 &
p=$!
now=0
for tp in $taps; do
	at=${tp%%:*}; xy=${tp#*:}
	[ $at -gt $now ] && sleep $((at - now)); now=$at
	echo "hold ${xy%,*} ${xy#*,} 150" > /tmp/n72-tap
	echo "tap $xy at $at s" >> $d/taps
done
[ $t -gt $now ] && sleep $((t - now))
cat /sys/kernel/debug/apple-mbx/stats > /dev/null 2>&1
sleep 10
grep -E "queued frames" /sys/kernel/debug/apple-mbx/stats > $d/gpu.txt 2>/dev/null
kill -USR2 $p 2>/dev/null; sleep 1; cat /proc/$p/maps > $d/maps.txt 2>/dev/null; kill -9 $p 2>/dev/null
sleep 1
echo "panel $(cat /sys/devices/platform/soc/38900000.lcd/panel_power)"
cat $d/gpu.txt
grep "n72 threads" $d/log | tail -3
grep FPS $d/log | awk '{printf "%s ", $NF}'; echo
