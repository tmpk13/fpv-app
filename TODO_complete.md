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

## Radio route (2026-09-02)

Replacing the forwarded-RTP receive path with a real one: devourer over USB and
an independent wfb-ng link layer, so the app is the ground station rather than
a viewer of someone else's.

### Licensing

- [x] Relicensed to GPL-2.0-only, which is what linking devourer requires
- [x] Own sources dual licensed `MIT OR GPL-2.0-only`, SPDX line on every file
- [x] `--no-default-features` builds the MIT viewer with no devourer in it
- [x] devourer and libusb as submodules pinned to exact commits, so the
      complete corresponding source of a binary is this tree
- [x] LICENSING.md: what is combined, why wfb-ng's GPL-3.0 cannot be, and how
      the LGPL relinking obligation is met

### wfb-ng link layer, in Rust

- [x] 802.11 filtering on the "WB" signature and the 32-bit channel id
- [x] Session packets: `crypto_box` open with gs.key, epoch and channel id
      checks, FEC parameters, rekey without losing the counters
- [x] Data packets: the original ChaCha20-Poly1305, 8-byte nonce, block header
      as additional data
- [x] Reed-Solomon over GF(2^8) on a systematic Vandermonde matrix, bit
      compatible with zfec and therefore with wfb-ng
- [x] Block ring: in-order release without waiting for a block to complete,
      FEC only when a gap blocks progress, eviction rather than unbounded delay
- [x] Counters that separate the failure modes: frames heard against frames
      that were ours, decrypt failures, packets repaired, packets lost

### Driver and platform

- [x] C shim over devourer's C++ `IRtlDevice`, eight functions wide
- [x] build.rs builds devourer through CMake, the shim through cc, and libusb
      from source for Android
- [x] Chip backends selectable; the FPV set by default, all of them behind a
      feature
- [x] Desktop: adapter found by USB id, udev rule shipped
- [x] Android: `UsbManager` through JNI reflection - device list, permission
      request, `openDevice`, file descriptor - with no Java source and no dex
- [x] Permission granted by polling `hasPermission` rather than receiving the
      broadcast, which is what would have needed a Java class

### App

- [x] Source selector: radio or forwarded RTP, both configured and kept
- [x] Settings page: channel, bandwidth, link id, radio port, key file
- [x] Link page: signal, SNR, noise floor, per-antenna strength, session and
      FEC parameters, repairs and losses
- [x] "No video" card names which stage of the radio link failed
- [x] Config schema extended, and a config from before the radio still loads
- [x] Key file resolved per platform: beside the config on desktop, in the
      app's external files directory on Android

### Verification

- [x] 141 tests, including the link layer against libsodium and wfb-ng's own
      `fec.c`
- [x] Every one of the 495 ways to lose four of twelve fragments reconstructs
- [x] 20000 arbitrary frames through the parsers without a panic
- [x] Desktop builds with and without the radio feature
- [x] aarch64 and armv7 Android cdylibs link devourer and libusb statically,
      export `android_main`, and need no `libc++_shared`

## Not done

- No adapter was attached. The radio path is verified by construction and by
  the link layer's tests against the reference implementations, but nothing
  has confirmed that a real RTL8812AU delivers frames through this shim.
- Android never run on a device, so the USB permission flow is compile-
  verified only.
- On-screen rendering never confirmed visually - no working screenshot path in
  this environment.
- Never tested against the real air unit; only against synthetic streams and
  generated fixtures.
- Injection is not exposed. devourer can transmit, and a ground station that
  could send RC or an adaptive-link downlink would need it; this build only
  receives.
