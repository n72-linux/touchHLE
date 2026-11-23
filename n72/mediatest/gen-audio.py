#!/usr/bin/env python3
"""Write N72MediaTest's synthetic test tone (44.1 kHz, stereo, 16-bit WAV).

    python3 gen-audio.py OUT.wav [SECONDS]

Layout (SECONDS defaults to 3.5, so that with numberOfLoops 1 the loop
point and the natural end both come before the app's 8 s limit):

  0.000-0.006  click: a 6 ms 2 kHz burst, both channels (the only click;
               a missing or doubled start, or the loop point, is heard)
  0.25 -1.00   440 Hz, both channels
  1.00 -1.75   880 Hz, left only
  1.75 -2.50   660 Hz, right only
  2.50 -3.25   sweep 220 -> 1760 Hz (exponential), both channels
  3.25 -end    silence

Every tone has 10 ms fades, so the only click is the one at the start.
For a longer file the sections are stretched in proportion.
"""
import math
import struct
import sys
import wave

RATE = 44100


def main():
    out = sys.argv[1]
    total = float(sys.argv[2]) if len(sys.argv) > 2 else 3.5
    k = total / 3.5
    sections = [  # (start, end, kind, f0, f1, left, right)
        (0.25 * k, 1.00 * k, "tone", 440, 440, 1, 1),
        (1.00 * k, 1.75 * k, "tone", 880, 880, 1, 0),
        (1.75 * k, 2.50 * k, "tone", 660, 660, 0, 1),
        (2.50 * k, 3.25 * k, "sweep", 220, 1760, 1, 1),
    ]
    amp = 0.4
    fade = 0.010
    n = int(total * RATE)
    frames = bytearray()
    for i in range(n):
        t = i / RATE
        left = right = 0.0
        if t < 0.006:  # the click
            v = 0.9 * math.sin(2 * math.pi * 2000 * t) * (1 - t / 0.006)
            left = right = v
        for (s, e, kind, f0, f1, gl, gr) in sections:
            if s <= t < e:
                u = t - s
                d = e - s
                if kind == "tone":
                    ph = 2 * math.pi * f0 * u
                else:  # exponential sweep: phase is the integral of f(t)
                    r = math.log(f1 / f0)
                    ph = 2 * math.pi * f0 * d / r * (math.exp(r * u / d) - 1)
                g = amp * min(1.0, u / fade, (d - u) / fade)
                v = g * math.sin(ph)
                left += gl * v
                right += gr * v
        frames += struct.pack("<hh", int(left * 32767), int(right * 32767))
    with wave.open(out, "wb") as w:
        w.setnchannels(2)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(bytes(frames))


if __name__ == "__main__":
    main()
