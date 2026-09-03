#!/usr/bin/env bash
# Send a synthetic RTP video stream to the port drone-app listens on, so the
# app can be developed and tested without the air unit powered up.
#
# This stands in for the tail of drone-cam's pipeline: `wfb_rx -u 5600` puts
# RTP on a local UDP port, and so does this. Everything downstream of that port
# is identical, which is the point - the app cannot tell the difference.
#
#   ./tools/fake-stream.sh              # H.265, 1280x720, 30 fps, to 5600
#   CODEC=h264 ./tools/fake-stream.sh   # the other codec
#   PORT=5601 SIZE=640x480 ./tools/fake-stream.sh
#   LOSS=5 ./tools/fake-stream.sh       # drop 5% of packets on the way
#
# Ctrl-C to stop.

set -euo pipefail

CODEC="${CODEC:-h265}"
PORT="${PORT:-5600}"
HOST="${HOST:-127.0.0.1}"
SIZE="${SIZE:-1280x720}"
FPS="${FPS:-30}"
BITRATE="${BITRATE:-4M}"
# Keyframe interval. Short, because the app waits for a parameter set before it
# starts decoding, and a long interval makes that wait look like a fault.
GOP="${GOP:-30}"
# Percentage of packets to discard, for exercising the loss counters. Needs
# netem and root, so it is off unless asked for.
LOSS="${LOSS:-0}"

case "$CODEC" in
    h264) encoder=libx264; payload_type=96 ;;
    h265) encoder=libx265; payload_type=97 ;;
    *) echo "CODEC must be h264 or h265, not '$CODEC'" >&2; exit 1 ;;
esac

command -v ffmpeg >/dev/null || { echo "ffmpeg is not installed" >&2; exit 1; }
# The encoder list is captured before being searched rather than piped into
# `grep -q`: grep exits at the first match, ffmpeg takes SIGPIPE for it, and
# under `pipefail` that failure becomes the pipeline's, so the check reports
# every encoder as missing.
encoders=$(ffmpeg -hide_banner -encoders 2>/dev/null || true)
case "$encoders" in
    *" $encoder "*) ;;
    *) echo "this ffmpeg has no $encoder" >&2; exit 1 ;;
esac

if [ "$LOSS" != "0" ]; then
    echo "note: LOSS needs netem on the loopback interface, e.g."
    echo "  sudo tc qdisc add dev lo root netem loss ${LOSS}%"
    echo "  sudo tc qdisc del dev lo root"
fi

echo "sending $CODEC $SIZE @ ${FPS}fps to $HOST:$PORT (Ctrl-C to stop)"

# -re paces the output at real time, which matters: without it ffmpeg sends the
# whole stream as fast as the socket accepts and the app sees one enormous
# burst rather than a live feed.
#
# The test pattern is the moving one rather than a static card, so a frozen
# picture is obvious at a glance - a still image looks identical whether it is
# updating or not, which is exactly the failure being watched for.
exec ffmpeg -hide_banner -loglevel warning \
    -re \
    -f lavfi -i "testsrc2=size=$SIZE:rate=$FPS" \
    -c:v "$encoder" \
    -preset ultrafast \
    -tune zerolatency \
    -b:v "$BITRATE" \
    -g "$GOP" \
    -payload_type "$payload_type" \
    -f rtp "udp://$HOST:$PORT"
