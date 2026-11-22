#!/usr/bin/env python3
"""prof-report.py DIR [LINES]: the N72_PROF sampler's dump as a table of functions.

DIR holds prof.bin (pairs of u32: pc, lr; kill -USR2 writes it), maps.txt
(/proc/PID/maps of the same process) and symbol tables as "addr type name"
lines:
  hle.syms   nm -n -C --defined-only target/.../touchHLE | grep -i " [tw] "
  gles.syms  nm -n --defined-only n72-gles/build/libn72gles.so.1.syms | grep -i " [tw] "
  musl.syms  nm -n -D --defined-only <the device's /lib/ld-musl-armhf.so.1> | grep -i " [tw] "
             (dynamic symbols only: a static function is charged to the exported one before it)
  gof.syms   nm -n -arch armv6 <the app's executable> | grep -i " t " | c++filt  (the guest; any name)
Each line: share of the samples, module, function, and its two commonest callers (by lr).
The profile is wall-clock, of the guest's thread; keep printf-heavy switches
(N72_GLES_STATE_STATS, N72_GLES_STATS) off while sampling."""
import sys, struct, bisect, collections, re
d = sys.argv[1]
def load(p, base=0):
    a = []
    for l in open(p, errors='replace'):
        m = re.match(r'([0-9a-f]+) \S (.*)', l.strip())
        if m: a.append((int(m.group(1), 16) + base, m.group(2)))
    a.sort()
    return [x[0] for x in a], [x[1] for x in a]
maps = []
for l in open(d + '/maps.txt'):
    f = l.split()
    lo, hi = [int(x, 16) for x in f[0].split('-')]
    maps.append((lo, hi, f[5] if len(f) > 5 else ''))
gles_base = min(lo for lo, hi, n in maps if 'libn72gles' in n)
musl_base = min(lo for lo, hi, n in maps if 'ld-musl' in n)
musl = load(d + '/musl.syms', musl_base)
hle = load(d + '/hle.syms'); gles = load(d + '/gles.syms', gles_base); gof = load(d + '/gof.syms')
def sym(tab, a):
    i = bisect.bisect_right(tab[0], a) - 1
    return tab[1][i] if i >= 0 else '?'
def where(pc):
    a = pc & ~1
    if a < 0x60000000:
        if a < 0x1a7000: return 'guest', sym(gof, a)
        return 'guest-other', '%08x' % (a & ~0xfff)
    for lo, hi, n in maps:
        if lo <= a < hi:
            if 'touchHLE' in n: return 'touchHLE', sym(hle, a)
            if 'libn72gles' in n: return 'n72gles', sym(gles, a)
            if 'ld-musl' in n: return 'musl', sym(musl, a)
            return n.split('/')[-1] or 'anon', '%08x' % (a - lo)
    return 'kernel/other', '%08x' % a
raw = open(d + '/prof.bin', 'rb').read()
n = len(raw) // 8
mods = collections.Counter(); syms = collections.Counter(); callers = collections.defaultdict(collections.Counter)
for i in range(n):
    pc, lr = struct.unpack_from('<II', raw, i * 8)
    m, s = where(pc)
    mods[m] += 1; syms[(m, s)] += 1
    callers[(m, s)][where(lr)] += 1
print('%d samples (10 ms each)' % n)
for m, c in mods.most_common(): print('%6.1f%%  %s' % (100.0 * c / n, m))
print()
for (m, s), c in syms.most_common(int(sys.argv[2]) if len(sys.argv) > 2 else 45):
    top = ', '.join('%s:%s %d' % (cm[:5], cs[:50], cc) for (cm, cs), cc in callers[(m, s)].most_common(2))
    print('%5.1f%% %-8s %-70s <- %s' % (100.0 * c / n, m, s[:70], top))
