#!/usr/bin/env bash
# Regenerates the audio fixtures read by the tests in
# interfaces/studio/audio_analysis/src/audio_loading.rs.
set -euo pipefail

command -v ffmpeg >/dev/null || { echo "generate.sh: ffmpeg not found on PATH" >&2; exit 1; }
cd "$(dirname "$0")"

gen() { # <lavfi source> <output file> <encoder args...>
  local source=$1 out=$2
  shift 2
  ffmpeg -nostdin -y -loglevel error -f lavfi -i "$source" \
    -fflags +bitexact -flags:a +bitexact "$@" "$out"
}

sine44=sine=frequency=440:sample_rate=44100:duration=1
sine16=sine=frequency=440:sample_rate=16000:duration=2

gen "$sine44" sine_16_44100.wav  -c:a pcm_s16le
gen "$sine44" sine_16_44100.aiff -c:a pcm_s16be
gen "$sine44" sine_16_44100.flac -c:a flac
gen "$sine44" sine_16_44100.mp3  -c:a libmp3lame
# ffmpeg's built-in vorbis encoder is experimental and accepts only stereo.
gen "$sine44" sine_16_44100.ogg  -ac 2 -c:a vorbis -strict -2

gen "$sine16" sine_8_16000.wav  -c:a pcm_u8
gen "$sine16" sine_8_16000.aiff -c:a pcm_s8
# ffmpeg's flac encoder has no 8-bit output, so this file is 16-bit.
gen "$sine16" sine_8_16000.flac -c:a flac
gen "$sine16" sine_8_16000.mp3  -c:a libmp3lame
gen "$sine16" sine_8_16000.ogg  -ac 2 -c:a vorbis -strict -2

# Left +0.5, right -0.5: both are exact in 16-bit PCM, as the tests' 1e-10 tolerance needs.
gen 'aevalsrc=0.5|-0.5:s=44100:d=1' constant_2ch_16_44100.wav -c:a pcm_s16le
