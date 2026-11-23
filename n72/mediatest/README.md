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

    movie FILE    MPMoviePlayerController on the bundle file FILE, then play

The default `test.txt` plays `Cut1.mov` twice, so you can try skipping the
movie with a tap on the second run. Unknown actions are logged and skipped.
`audio FILE` (AVAudioPlayer with MP3/AAC/ALAC) is planned and would go in
`runNextStep:` in `main.m`.

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

## Install and run on the iPod

Copy the app into the touchHLE apps folder and give it a movie:

    scp -r OUT_DIR/N72MediaTest.app root@IPOD:/root/n72hle-test/touchHLE_apps/
    ssh root@IPOD 'cd /root/n72hle-test/touchHLE_apps &&
      cp Spore.app/Data/Movies/Cut1.mov N72MediaTest.app/Cut1.mov'
    ssh root@IPOD 'cd /root/n72hle-test && ./touchHLE touchHLE_apps/N72MediaTest.app'

To change the steps, edit `N72MediaTest.app/test.txt` on the device. The
steps can name any file copied into the .app.

## Expected log

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

If touchHLE cannot play the movie (no `n72-player` in PATH, no display, not
a cpu-native build, file in an .ipa), it fakes the end after 1 s. A
"finished after 1.0x s" means it did not really play: look for the `n72:`
lines above it. The message `movie Cut1.mov: not in the bundle` plus a red
window means the file was not copied in.
