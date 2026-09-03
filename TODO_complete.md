# Completed

## Initial build (2026-09-02)

An egui app showing the drone-cam video stream, for desktop and Android.

### Video path

- [x] UDP receive with a bounded read timeout, so a settings change and
      shutdown are both noticed without a second wakeup mechanism
- [x] RTP header parsing: CSRC lists, extension headers, padding
- [x] H.264 depayloading: single NAL, STAP-A aggregates, FU-A fragments
- [x] H.265 depayloading: single NAL, AP aggregates, FU fragments
- [x] Access unit assembly on timestamp change and on the marker bit
- [x] Sequence tracking: loss, reordering, duplicates, 16-bit wraparound, and
      large jumps read as a stream restart rather than as mass loss
- [x] Continuous codec detection by voting, ported from drone-cam's
      `codec_probe.py`
- [x] Wait for a parameter set before starting the decoder
- [x] Automatic decoder rebuild when packets arrive but nothing decodes, which
      is what an air unit rebooted into the other codec looks like
- [x] Desktop decode: GStreamer `appsrc ! parse ! decode ! videoconvert !
      appsink`, tuned for latency (live source, no clock sync, one-deep
      dropping queue)
- [x] Pipeline errors surfaced from a bus thread rather than a glib main loop
      that this app does not run
- [x] Android decode: NDK `AMediaCodec`, configured from the stream's own
      parameter sets, with the codec confined to its own thread
- [x] YUV to RGBA for Android output: I420, NV12, NV21, with stride, slice
      height, crop rectangle and both BT.601 and BT.709 matrices
- [x] Single-slot frame mailbox, so a slow UI drops pictures instead of
      accumulating latency

### App

- [x] One crate, two targets: desktop `[[bin]]` and Android `cdylib` with
      `android_main`
- [x] Video page: full-screen picture, fit/fill on tap, floating readout
- [x] "No video" card that names which of the four faults it is
- [x] Link page: packets, loss, reordering, restarts, decode errors, UI drops,
      timing, and a minute of bitrate and loss history
- [x] Link history graph, hand-painted, with loss on a fixed ceiling so a
      healthy link does not look alarming
- [x] Settings page editing a draft, applied on save so a half-typed port never
      rebinds the socket
- [x] Menu page and animated corner toggle
- [x] Safe-area insets on Android, re-queried each frame for rotation
- [x] TOML config, saved in place so comments and unknown keys survive
- [x] Sizing in fractions of screen and text throughout, with one absolute
      touch-target floor

### Verification

- [x] 73 unit tests: RTP, codec detection, YUV conversion, config round trip,
      picture placement, stall detection, formatting
- [x] 5 end-to-end tests over a real RTP stream from ffmpeg, both codecs
- [x] `tools/fake-stream.sh` synthetic source standing in for `wfb_rx -u`
- [x] Android arm64 and armv7 compile; arm64 cdylib links, exports
      `android_main`, binds `libmediandk.so`
- [x] README with a mermaid architecture diagram

## Not done

- On-screen rendering never confirmed visually - no working screenshot path in
  this environment. See NOTES.md.
- Android never run on a device.
- Never tested against the real air unit; only against synthetic streams.
