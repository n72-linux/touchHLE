#!/bin/sh
# gofend.sh: after gofrun.sh runs: brightness value back, n72-ui started again (it owns the panel and backlight power).
if [ -f /root/gof/brightness.saved ]; then
	for f in /sys/class/backlight/*/brightness; do cat /root/gof/brightness.saved > $f; done
	rm -f /root/gof/brightness.saved
fi
p=$(pidof n72-tapd) && kill $p
pidof n72-ui >/dev/null || rc-service n72-ui start
