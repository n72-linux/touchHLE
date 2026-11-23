#!/bin/sh
# Make N72MediaTest's audio test files on the Mac (synthetic tones only;
# never commit them):
#   sh gen-audio.sh OUT_DIR [SECONDS]
# Needs python3, lame (Homebrew) and afconvert (macOS). Writes into
# OUT_DIR/media/: tone.wav (16-bit PCM), tone.mp3 (CBR 128k), tone-aac.m4a
# (AAC-LC), tone-heaac.m4a (HE-AAC v1), tone-heaac2.m4a (HE-AAC v2),
# tone-alac.m4a (ALAC 16-bit), all 44.1 kHz stereo, plus OUT_DIR/
# test-movie.txt and OUT_DIR/test-audio.txt (step files to copy over the
# app's test.txt).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:?usage: gen-audio.sh OUT_DIR [SECONDS]}
secs=${2:-3.5}
m=$out/media
mkdir -p "$m"
rm -f "$m"/tone*

python3 "$here/gen-audio.py" "$m/tone.wav" "$secs"
lame --quiet -b 128 --cbr -m s --resample 44.1 "$m/tone.wav" "$m/tone.mp3"
afconvert -f m4af -d aac  -s 0 -b 128000 "$m/tone.wav" "$m/tone-aac.m4a"
afconvert -f m4af -d aach            "$m/tone.wav" "$m/tone-heaac.m4a"
afconvert -f m4af -d aacp            "$m/tone.wav" "$m/tone-heaac2.m4a"
afconvert -f m4af -d alac            "$m/tone.wav" "$m/tone-alac.m4a"

cp "$here/test.txt" "$out/test-movie.txt"
{
  echo "# N72MediaTest audio steps: copy over N72MediaTest.app/test.txt"
  echo "# (with the media/ files copied into the .app). audio FILE SECONDS"
  for f in tone.wav tone.mp3 tone-aac.m4a tone-heaac.m4a tone-heaac2.m4a \
           tone-alac.m4a; do
    echo "audio $f 8"
  done
} > "$out/test-audio.txt"

for f in "$m"/tone*; do afinfo "$f" | grep -E "File:|Data format|estimated duration|bit rate" ; done
