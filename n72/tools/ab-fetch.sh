#!/bin/sh
# ab-fetch.sh [DEVICE_DIR...]: copy gofab.sh results from the iPod to "$PWD"/ab-results/.
# Default: every directory under /root/gof/ab not fetched yet. Prints the report of each.
# N72_HOST: the iPod's address (default 192.168.7.2, USB). Profiles of a run:
#   ab-prof.sh ab-results/<dir>/runs/<run> KIT NAME
W=$(cd "$(dirname "$0")/../../.." && pwd)
T=$(cd "$(dirname "$0")" && pwd)
O="-i $W/n72-ssh-key -o ConnectTimeout=20 -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR"
H=root@${N72_HOST:-192.168.7.2}
mkdir -p $W/ab-results
new=
if [ $# -eq 0 ]; then
	new=1; set -- $(ssh $O $H 'ls -d /root/gof/ab/*/ 2>/dev/null')
fi
for d; do
	b=$(basename $d)
	[ -n "$new" ] && [ -d $W/ab-results/$b ] && continue
	mkdir -p $W/ab-results/$b
	ssh $O $H "tar cf - -C /root/gof/ab $b" | tar xf - -C $W/ab-results || { echo "$b: fetch failed"; continue; }
	echo "== $b"; cat $W/ab-results/$b/info.txt
	python3 -I $T/ab-report.py $W/ab-results/$b/results.tsv
done
