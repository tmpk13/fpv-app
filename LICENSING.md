# Licensing

drone-app used to be MIT. Linking [devourer][devourer] changed that, and this
file records exactly what the combined work is and why, because the answer is
not the one a single SPDX field can carry.

## The short version

**A drone-app binary built with the `radio` feature - the default - is
GPL-2.0-only.** That is what `Cargo.toml` declares. Distributing one obliges
you to offer the complete corresponding source of the whole thing, including
the pinned submodules under `third_party/`.

**Every file this project wrote itself is dual licensed `MIT OR
GPL-2.0-only`,** and every one of them carries that as an SPDX line. So the
parts worth reusing elsewhere - the RTP depayloader, the YUV conversion, the
wfb-ng link layer - stay reusable under MIT by anyone who takes them out of
here. It is only the *combination* with devourer that is GPL.

Building with `--no-default-features` leaves out devourer entirely and gives
back the old RTP-over-UDP viewer, which is MIT throughout.

## What is combined, and under what terms

| component | where | license | why it is compatible |
| --- | --- | --- | --- |
| drone-app's own code | `src/`, `tests/`, `tools/` | `MIT OR GPL-2.0-only` | MIT is GPL compatible; the GPL option is what the combined work uses |
| devourer | `third_party/devourer` (submodule) | GPL-2.0, no "or later" | the strongest term in the tree; it sets the license of the whole binary |
| libusb | `third_party/libusb` (submodule) | LGPL-2.1-or-later | LGPL-2.1 permits relicensing to GPL-2.0; see the relinking note below |
| zfec's Reed-Solomon algorithm | reimplemented in `src/wfb/fec.rs` | GPL-2.0-or-later (zfec), BSD-3-Clause (Rizzo's original) | both permit use under GPL-2.0 |

### devourer is GPL-2.0-only, and that is the binding constraint

devourer's `LICENSE` is the plain GPLv2 text and its README says "GPL-2.0"
with no "or, at your option, any later version". A GPL-2.0-only work cannot be
combined into a GPL-3.0 one, so the combined binary here is GPL-2.0-only and
cannot be upgraded to GPL-3.0.

### Which is why no wfb-ng code is in this repository

[wfb-ng][wfb-ng] is GPL-3.0-only. **GPL-3.0-only and GPL-2.0-only are mutually
incompatible**, so copying any part of wfb-ng into a binary that links
devourer would produce something undistributable.

`src/wfb/` is therefore an independent implementation, written against the
protocol wfb-ng documents in the comment block at the top of
`src/wifibroadcast.hpp` - packet types, header layouts, nonce construction,
and which AEAD covers what. Protocol descriptions are not the expressive part
of a program, and none of wfb-ng's code was copied. The behaviour is meant to
be identical on the wire; the code is not a translation of theirs.

`tools/gen_wfb_fixtures.py` does read a wfb-ng checkout, but only at
development time, to generate the test vectors under `tests/fixtures/`. It is
never built into anything and the fixtures it emits are data, not code.

### The FEC is a port, and is credited as one

wfb-ng's Reed-Solomon comes from [zfec][zfec], which is itself derived from
Luigi Rizzo's `fec`. zfec is offered under "the GNU General Public License,
version 2 or, at your option, any later version", and Rizzo's original under a
three-clause BSD licence. Both permit use under GPL-2.0, so unlike the rest of
wfb-ng this piece *could* be taken directly.

`src/wfb/fec.rs` is a Rust implementation of that algorithm - the same
GF(2^8) field with primitive polynomial `x^8+x^4+x^3+x^2+1`, the same
systematic Vandermonde generator matrix - and its header credits zfec and
Rizzo. It is bit-compatible by construction and by test, and it is GPL-2.0
here.

### libusb and the LGPL relinking obligation

libusb is LGPL-2.1-or-later and is linked statically, which the LGPL allows on
the condition that a recipient can relink the application against a modified
libusb. Shipping this repository satisfies that: `third_party/libusb` is the
unmodified upstream source at a pinned tag, and `build.rs` compiles it from
that source, so anyone can substitute their own copy and rebuild.

Section 3 of the LGPL-2.1 also permits taking a copy under GPL-2.0 outright,
which is the reading that makes the combination with a GPL-2.0-only work
unambiguous.

## Complete corresponding source

The submodules are pinned to exact commits, recorded in this repository's git
index:

```sh
git submodule update --init third_party/devourer third_party/libusb
git -C third_party/devourer log -1 --format=%H   # the devourer revision built
git -C third_party/libusb  log -1 --format=%H    # the libusb revision built
```

Distributing an APK or a desktop binary built from this tree means offering
that source too - this repository plus those two revisions is the whole of it.
Nothing is downloaded during the build.

## Attribution

- **devourer** - OpenIPC. <https://github.com/OpenIPC/devourer>. The userspace
  Realtek driver this project's radio path is built on.
- **wfb-ng** - Vasily Evseenko and contributors. <https://github.com/svpcom/wfb-ng>.
  The protocol `src/wfb/` speaks, and the reference the fixtures are generated
  from.
- **zfec** - Zooko Wilcox-O'Hearn / Allmydata, after Luigi Rizzo.
  <https://tahoe-lafs.org/trac/zfec/>. The erasure code.
- **libusb** - <https://libusb.info>.
- **PixelPilot** - OpenIPC. <https://github.com/OpenIPC/PixelPilot>. The prior
  art that showed an unrooted Android phone can be the whole ground station.

[devourer]: https://github.com/OpenIPC/devourer
[wfb-ng]: https://github.com/svpcom/wfb-ng
[zfec]: https://tahoe-lafs.org/trac/zfec/
