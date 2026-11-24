# N72MediaTest

A tiny iPhone OS 3 app for trying touchHLE's media support (n72 fork)
without navigating a game. It shows its own window (green), then runs the
steps in its bundle file `test.txt`, 3 s apart, and logs each with NSLog
(prefix `N72MediaTest:`).

The app is portrait (`UIInterfaceOrientationPortrait`, status bar hidden).
Movies play landscape with the home button on the right, as on iPhone OS 3:
touchHLE's default for a portrait app (n72-player turn 270).

## Steps (`test.txt`)

One step per line, `#` lines and empty lines ignored:

    movie FILE            MPMoviePlayerController on the bundle file FILE, then play
    audio FILE [SECONDS]  AVAudioPlayer on FILE: numberOfLoops 1, play; stopped
                          after SECONDS (default 8) or by
                          audioPlayerDidFinishPlaying:successfully:

The app only reads `test.txt`. The bundled one is the movie test: it plays
`Cut1.mov` twice, so you can try a tap-to-skip on the second run. For the
audio test, copy `test-audio.txt` (written by `gen-audio.sh`) over it.
Unknown actions are logged and skipped. New actions go in `runNextStep:` in
`main.m`.

An audio step checks the time and `isPlaying` every 0.25 s. When
`isPlaying` turns NO, it only logs it, because the last 0.5 s buffers are
still queued and not yet heard. The step ends at the time limit or on the
delegate call. touchHLE does not call AVAudioPlayer delegates yet (its
`setDelegate:` is a logged TODO), so on touchHLE every audio step ends with
"stopped at the time limit".

Window colours: green before the first step, blue after a step finished,
red after a step failed (file not in the bundle, unknown action).

## Build (on the Mac)

    sh build.sh OUT_DIR

This needs the Docker image `ios-armv6-tc` (see `ios-tc/README.md`). There
is no iOS SDK: `decls.h` declares the few classes and functions used, and
`stubs/*.tbd` export exactly the symbols the binary imports. If you use a
new class, function or constant, add it to both and check that touchHLE
implements it. The build produces `OUT_DIR/N72MediaTest.app`, containing
`N72MediaTest` (armv6, ldid-signed), `Info.plist`, `test.txt` and `PkgInfo`.
Never commit the built .app or any media.

### Audio test files

    sh gen-audio.sh OUT_DIR

This uses `gen-audio.py` (python3), `lame` (Homebrew) and `afconvert`
(macOS) to write synthetic tones into `OUT_DIR/media/`, all 44.1 kHz
stereo:
- `tone.wav`: 16-bit PCM
- `tone.mp3`: lame, CBR 128k
- `tone-aac.m4a`: AAC-LC, `afconvert -d aac`
- `tone-heaac.m4a`: HE-AAC v1, `afconvert -d aach`
- `tone-heaac2.m4a`: HE-AAC v2, `afconvert -d aacp`
- `tone-alac.m4a`: ALAC from 16-bit, `afconvert -d alac`

It also writes `OUT_DIR/test-audio.txt` (one `audio NAME 8` per file, in
that order) and `OUT_DIR/test-movie.txt` (a copy of the bundled
`test.txt`).

Each file is 3.5 s long by default, so that with one loop both the loop
point (3.5 s) and the natural end (7 s) fall inside the 8 s limit. For
10 s files, run `sh gen-audio.sh OUT_DIR 10` and raise the step seconds
to about 22.

## Install and run on the iPod

Copy the app into the touchHLE apps folder and give it a movie:

    scp -r OUT_DIR/N72MediaTest.app root@IPOD:/root/n72hle-test/touchHLE_apps/
    ssh root@IPOD 'cd /root/n72hle-test/touchHLE_apps &&
      cp Spore.app/Data/Movies/Cut1.mov N72MediaTest.app/Cut1.mov'
    ssh root@IPOD 'cd /root/n72hle-test && ./touchHLE touchHLE_apps/N72MediaTest.app'

For the audio test:

    scp OUT_DIR/media/* root@IPOD:/root/n72hle-test/touchHLE_apps/N72MediaTest.app/
    scp OUT_DIR/test-audio.txt root@IPOD:/root/n72hle-test/touchHLE_apps/N72MediaTest.app/test.txt

To go back to the movie test, copy `test-movie.txt` over `test.txt` the
same way. To change the steps, edit `N72MediaTest.app/test.txt` on the device. The
steps can name any file copied into the .app.

## Expected log

The binary is linked for iPhone OS 3.0 (classic relocations; see the
comment in `build.sh` for why not 3.1+), so touchHLE prints no
`dyld_stub_binder` warnings. The first step starts 3 s after launch, so the
green window shows first.

    N72MediaTest[...] N72MediaTest: launched
    N72MediaTest[...] N72MediaTest: 2 step(s) in test.txt
    N72MediaTest[...] N72MediaTest: movie Cut1.mov start
    TODO: [(MPMoviePlayerController*)... initWithContentURL:...]
    n72: movie .../Cut1.mov playing in n72-player, pid ..., turn 270
    n72: movie ended (...), n72-player pid ...: ...
    N72MediaTest[...] N72MediaTest: movie finished after 12.34 s
    N72MediaTest[...] N72MediaTest: movie Cut1.mov start
    ...                      (second run: tap to skip)
    N72MediaTest[...] N72MediaTest: movie finished after 2.10 s
    N72MediaTest[...] N72MediaTest: done

The app then keeps running (blue window) until you quit touchHLE.

Audio test, for each file:

    N72MediaTest: audio tone.mp3 start
    N72MediaTest: audio tone.mp3 duration 3.527 s, loops 1, limit 8.0 s
    N72MediaTest: audio isPlaying NO after 6.75 s
    N72MediaTest: audio stopped at the time limit after 8.00 s

What you should hear, each file the same:
- one short click at the start
- 440 Hz (A4) on both sides
- 880 Hz on the left only
- 660 Hz on the right only
- a rising sweep from 220 to 1760 Hz
- a short silence, then exactly one click as it loops
- the whole sequence once more, then silence until the step ends at 8 s

Signs of trouble:
- No click, or two clicks, at the start or at the loop point.
- Tones too low or slow, about an octave down: the file was played at the
  wrong sample rate.
- Swapped left and right.
- HE-AAC: dull, missing the top of the sweep (only the 22.05 kHz AAC core
  is decoded, without SBR), or at half speed.
- HE-AAC v2: mono, if parametric stereo is not decoded.

touchHLE decodes through symphonia with AAC-LC only (`Cargo.toml`: the
"aac" feature), so the two HE-AAC files are the ones expected to show this.
A file it cannot decode at all gives "cannot open, error code ..." and a
red window.

If touchHLE cannot play the movie (no `n72-player` in PATH, no display, not
a cpu-native build, file in an .ipa), it fakes the end after 1 s. A
"finished after 1.0x s" means it did not really play: look for the `n72:`
lines above it. The message `movie Cut1.mov: not in the bundle` plus a red
window means the file was not copied in.
