#!/usr/bin/env python3
"""ab-report.py RESULTS.tsv [BASELINE]: gofab.sh's runs as a table per route and variant.

Per variant: runs, the median of the runs' median FPS, the range of the run medians, the
busiest thread's CPU ms a frame (the game's main thread), the other threads', GPU TA and
render us a frame (apple-mbx, over the window). Against BASELINE (default: the first
variant): the change of the median, and whether the two ranges of run medians are apart
("apart": every run of one faster than every run of the other; with 3+ runs each that is
worth believing, with fewer it is a hint). Several files: give them in turn."""
import sys, statistics, collections

def load(path):
    rows = []
    with open(path) as f:
        head = f.readline().rstrip('\n').split('\t')
        for l in f:
            v = l.rstrip('\n').split('\t')
            if len(v) == len(head):
                rows.append(dict(zip(head, v)))
    return rows

def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    files = [a for a in sys.argv[1:] if a.endswith('.tsv')]
    names = [a for a in sys.argv[1:] if not a.endswith('.tsv')]
    rows = [r for f in files for r in load(f)]
    by = collections.OrderedDict()
    for r in rows:
        if float(r['fps_med']) <= 0:
            continue
        by.setdefault(r['route'], collections.OrderedDict()).setdefault(r['name'], []).append(r)
    for route, vs in by.items():
        base = names[0] if names and names[0] in vs else next(iter(vs))
        print('%s (baseline %s)' % (route, base))
        print('  %-18s %4s %7s %15s %8s %8s %7s %7s  %s' % ('variant', 'runs', 'FPS', 'run medians', 'main ms', 'other', 'TA us', 'rnd us', 'vs baseline'))
        bm = [float(r['fps_med']) for r in vs[base]]
        for name, rs in vs.items():
            m = [float(r['fps_med']) for r in rs]
            def med(k):
                return statistics.median(float(r[k]) for r in rs)
            cmp = ''
            if name != base:
                d = (statistics.median(m) / statistics.median(bm) - 1) * 100
                apart = min(m) > max(bm) or max(m) < min(bm)
                cmp = '%+5.1f %%  %s' % (d, 'apart' if apart else 'overlap')
            print('  %-18s %4d %7.2f %6.2f - %6.2f %8.1f %8.1f %7.0f %7.0f  %s' % (
                name, len(rs), statistics.median(m), min(m), max(m),
                med('main_ms_frame'), med('other_ms_frame'), med('ta_us'), med('render_us'), cmp))
        print()

main()
