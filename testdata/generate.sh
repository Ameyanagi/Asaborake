#!/usr/bin/env bash
#
# Generate a synthetic "programme with commercials" for testing.
#
# Asaborake ships no broadcast recordings — they are copyrighted, and a repo
# that carried them could not be public. Instead the test material is built
# here from ffmpeg primitives, with the structure real broadcast has and that
# detection depends on:
#
#   * a semi-transparent station logo in the top-left, present through the
#     programme and absent through the commercials;
#   * commercial blocks on the 15-second grid;
#   * near-silence and a hard cut at every junction;
#   * fades to black inside the programme, which is where the logo fit gets
#     the flat-background frames it needs.
#
# Usage: testdata/generate.sh OUTPUT.ts [PROGRAMME_SECONDS]

set -euo pipefail

output="${1:-testdata/generated/sample.ts}"
programme="${2:-60}"

mkdir -p "$(dirname "$output")"

# Layout: programme, 30s of commercials, programme, 30s of commercials,
# programme. Each programme block is a third of the requested length.
block=$((programme / 3))
cm=30

fps=30
width=640
height=360

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# A logo: white text on transparent, which composites the way a real station
# mark does rather than replacing the picture.
ffmpeg -hide_banner -loglevel error -y \
    -f lavfi -i "color=c=black@0.0:s=96x48:r=$fps:d=1,format=rgba" \
    -vf "drawtext=text='ABC':fontcolor=white@0.75:fontsize=34:x=8:y=6" \
    -frames:v 1 "$work/logo.png"

# One programme block: moving content interleaved with flat title cards, with
# the logo composited over all of it.
#
# The title cards are what make this material usable, and they are why real
# recordings work. Fitting the logo needs frames where the area around it is a
# single flat colour, at *several different brightnesses* — a run of frames
# that are all black leaves the fit's slope and intercept indistinguishable.
# Real programmes supply this constantly: title cards, fades, open sky, studio
# walls. Content that is busy from beginning to end teaches nothing.
make_programme() {
    local out="$1" seconds="$2" seed="$3"
    local card=$((seconds / 6))
    [ "$card" -lt 2 ] && card=2

    # Grey levels spread across the range, one per block, so that across the
    # whole recording the fit sees a wide spread of backgrounds.
    local dark=$((20 + seed * 10))
    local light=$((150 + seed * 25))

    ffmpeg -hide_banner -loglevel error -y \
        -f lavfi -i "mandelbrot=size=${width}x${height}:rate=$fps:maxiter=200" -t "$seconds" \
        -f lavfi -i "sine=frequency=$((300 + seed * 50)):sample_rate=48000:duration=$seconds" \
        -i "$work/logo.png" \
        -f lavfi -i "color=c=0x$(printf '%02x%02x%02x' $dark $dark $dark):s=${width}x${height}:r=$fps:d=$card" \
        -f lavfi -i "color=c=0x$(printf '%02x%02x%02x' $light $light $light):s=${width}x${height}:r=$fps:d=$card" \
        -filter_complex "\
            [0:v]trim=0:$((seconds - card * 2)),setpts=PTS-STARTPTS[busy];\
            [busy][3:v][4:v]concat=n=3:v=1:a=0[bg];\
            [bg][2:v]overlay=12:10[v]" \
        -map "[v]" -map 1:a \
        -c:v mpeg2video -q:v 4 -c:a aac -b:a 128k -shortest \
        -f mpegts "$out"
}

# One commercial block: different content, no logo, and a moment of silence at
# each end, as broadcast inserts at a junction.
make_commercials() {
    local out="$1" seconds="$2"
    ffmpeg -hide_banner -loglevel error -y \
        -f lavfi -i "smptebars=size=${width}x${height}:rate=$fps:duration=$seconds" \
        -f lavfi -i "sine=frequency=900:sample_rate=48000:duration=$seconds" \
        -filter_complex "[1:a]volume=enable='between(t,0,0.4)+between(t,$((seconds - 1)),$seconds)':volume=0[a]" \
        -map 0:v -map "[a]" \
        -c:v mpeg2video -q:v 4 -c:a aac -b:a 128k \
        -f mpegts "$out"
}

make_programme "$work/p1.ts" "$block" 1
make_commercials "$work/c1.ts" "$cm"
make_programme "$work/p2.ts" "$block" 2
make_commercials "$work/c2.ts" "$cm"
make_programme "$work/p3.ts" "$block" 3

# The blocks are joined with the concat *filter* and encoded once, so the
# result has a single continuous timeline.
#
# Byte-concatenating the transport streams would be quicker, but each stretch
# would keep its own timebase starting near zero — and ffmpeg reinitialises its
# filter graph at every such discontinuity, restarting both the timestamp and
# the frame counter. Nothing downstream could then address a position in the
# recording. An EPGStation recording is one continuous capture, so this is also
# the more faithful shape.
ffmpeg -hide_banner -loglevel error -y \
    -i "$work/p1.ts" -i "$work/c1.ts" -i "$work/p2.ts" -i "$work/c2.ts" -i "$work/p3.ts" \
    -filter_complex "[0:v][0:a][1:v][1:a][2:v][2:a][3:v][3:a][4:v][4:a]concat=n=5:v=1:a=1[v][a]" \
    -map "[v]" -map "[a]" \
    -c:v mpeg2video -q:v 4 -c:a aac -b:a 128k \
    -f mpegts "$output"

echo "wrote $output"
echo "expected commercials: ${block}s-$((block + cm))s and $((block * 2 + cm))s-$((block * 2 + cm * 2))s"
