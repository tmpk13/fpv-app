//! YUV 4:2:0 to RGBA, for decoder output that arrives as raw planes.
//!
//! Only Android needs this - GStreamer's `videoconvert` does the same job on
//! desktop - but it is compiled and tested everywhere on purpose. It is the
//! part of the Android path most likely to be subtly wrong (a stride ignored,
//! a matrix swapped, a plane order reversed) and the part whose failure is
//! least obvious: the picture still appears, just with the colors wrong or a
//! diagonal skew. A phone is not needed to check any of that, so the tests run
//! on the desktop with the rest.
//!
//! Two things make this more than a matrix multiply:
//!
//! - **Stride.** `AMediaCodec` writes rows padded out to a hardware alignment,
//!   so a row is `stride` bytes even though only `width` of them are pixels.
//!   Reading it as tightly packed produces the classic diagonal shear.
//! - **Slice height.** The chroma planes start at `slice_height` rows, not at
//!   `height`, for the same reason. Getting this wrong puts the color planes
//!   at an offset and tints the picture in bands.
//!
//! Both are read from the codec's output format rather than assumed.

/// How the decoder laid out the three components.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layout {
    /// Three planes: all Y, then all U, then all V. Android's
    /// `COLOR_FormatYUV420Planar`.
    I420,
    /// Two planes: all Y, then U and V interleaved. Android's
    /// `COLOR_FormatYUV420SemiPlanar`, and what most hardware decoders emit.
    Nv12,
    /// Like [`Layout::Nv12`] but with the chroma pair the other way round.
    Nv21,
}

/// The YUV-to-RGB matrix a stream was encoded with.
///
/// Picking the wrong one is not catastrophic but is visible: greens shift and
/// skin tones go slightly wrong. There is no signalling for it in the RTP
/// stream, so [`ColorSpace::for_height`] guesses from the picture size, which
/// is the same rule every player uses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ColorSpace {
    /// Standard definition.
    Bt601,
    /// High definition, which is everything an FPV camera produces.
    Bt709,
}

impl ColorSpace {
    /// The convention: standard definition is BT.601, HD and above BT.709.
    pub fn for_height(height: u32) -> Self {
        if height >= 720 {
            ColorSpace::Bt709
        } else {
            ColorSpace::Bt601
        }
    }

    /// Coefficients as `(v_to_r, v_to_g, u_to_g, u_to_b)`, in 16.16 fixed
    /// point.
    ///
    /// Fixed point rather than floating: this runs per pixel on a phone, and
    /// at 1080p60 that is 124 million pixels a second. The rounding is well
    /// under one code value, so nothing is lost.
    const fn coefficients(self) -> (i32, i32, i32, i32) {
        match self {
            // 1.596, -0.813, -0.391, 2.018
            ColorSpace::Bt601 => (104_597, -53_279, -25_624, 132_251),
            // 1.793, -0.533, -0.213, 2.112
            ColorSpace::Bt709 => (117_506, -34_930, -13_959, 138_412),
        }
    }
}

/// Luma scale for limited-range video: 1.164, in 16.16 fixed point.
///
/// Decoder output is studio range, where black is 16 and white is 235 rather
/// than 0 and 255. Skipping this scale is the single most common mistake here
/// and shows up as washed-out blacks rather than as anything obviously broken.
const Y_SCALE: i32 = 76_309;

/// Black level and chroma zero point for limited-range video.
const Y_OFFSET: i32 = 16;
const C_OFFSET: i32 = 128;

/// A decoder's output buffer, described well enough to read pixels out of.
#[derive(Clone, Copy, Debug)]
pub struct PlaneLayout {
    /// Visible picture size, after cropping.
    pub width: u32,
    pub height: u32,
    /// Bytes per row of the luma plane, padding included.
    pub stride: u32,
    /// Rows between the start of the luma plane and the start of chroma,
    /// padding included.
    pub slice_height: u32,
    /// Offset of the visible picture inside the coded one.
    pub crop_x: u32,
    pub crop_y: u32,
    pub layout: Layout,
    pub color_space: ColorSpace,
}

impl PlaneLayout {
    /// Whether `buffer` is big enough to hold the planes this describes.
    ///
    /// Checked once per frame rather than bounds-checking per pixel: the
    /// conversion loop is the hot path, and a buffer that is short is a
    /// decoder bug rather than something to handle per sample.
    pub fn fits(&self, buffer: usize) -> bool {
        let stride = self.stride as usize;
        let slice = self.slice_height as usize;
        let luma = stride * slice;
        // Chroma is a quarter of the samples, laid out as half-height rows of
        // the same stride (interleaved) or half-stride (planar).
        let chroma_rows = slice.div_ceil(2);
        let needed = match self.layout {
            Layout::I420 => luma + 2 * (stride / 2) * chroma_rows,
            Layout::Nv12 | Layout::Nv21 => luma + stride * chroma_rows,
        };
        // The last chroma row of a picture whose height is not a multiple of
        // two is often not padded out, so allow the buffer to be short by less
        // than one row rather than rejecting a frame that is really there.
        buffer + stride >= needed && buffer >= luma
    }
}

/// Convert one decoded picture to tightly packed RGBA.
///
/// `out` is resized to `width * height * 4`. Returns `false` without touching
/// `out` if the buffer is too small for the layout, which is the one error
/// worth distinguishing: it means the format was misread, not that the frame
/// was bad.
pub fn to_rgba(buffer: &[u8], layout: &PlaneLayout, out: &mut Vec<u8>) -> bool {
    if layout.width == 0 || layout.height == 0 || !layout.fits(buffer.len()) {
        return false;
    }

    let (width, height) = (layout.width as usize, layout.height as usize);
    let stride = layout.stride as usize;
    let slice = layout.slice_height as usize;
    let (crop_x, crop_y) = (layout.crop_x as usize, layout.crop_y as usize);

    out.clear();
    out.resize(width * height * 4, 0);

    let (v_r, v_g, u_g, u_b) = layout.color_space.coefficients();
    let luma_len = stride * slice;
    let chroma = &buffer[luma_len.min(buffer.len())..];
    let chroma_stride = match layout.layout {
        Layout::I420 => stride / 2,
        Layout::Nv12 | Layout::Nv21 => stride,
    };
    // In planar layout the V plane follows the whole U plane.
    let v_plane_offset = chroma_stride * slice.div_ceil(2);

    for y in 0..height {
        let src_row = (y + crop_y) * stride;
        let chroma_row = ((y + crop_y) / 2) * chroma_stride;
        let dst_row = y * width * 4;

        for x in 0..width {
            let sx = x + crop_x;
            let luma = i32::from(buffer[src_row + sx]);

            // Chroma is subsampled by two in both directions, so one sample
            // serves a 2x2 block of pixels.
            let (u, v) = match layout.layout {
                Layout::I420 => {
                    let i = chroma_row + sx / 2;
                    let u = chroma.get(i).copied().unwrap_or(128);
                    let v = chroma.get(v_plane_offset + i).copied().unwrap_or(128);
                    (i32::from(u), i32::from(v))
                }
                Layout::Nv12 => {
                    let i = chroma_row + (sx / 2) * 2;
                    let u = chroma.get(i).copied().unwrap_or(128);
                    let v = chroma.get(i + 1).copied().unwrap_or(128);
                    (i32::from(u), i32::from(v))
                }
                Layout::Nv21 => {
                    let i = chroma_row + (sx / 2) * 2;
                    let v = chroma.get(i).copied().unwrap_or(128);
                    let u = chroma.get(i + 1).copied().unwrap_or(128);
                    (i32::from(u), i32::from(v))
                }
            };

            let c = (luma - Y_OFFSET) * Y_SCALE;
            let d = u - C_OFFSET;
            let e = v - C_OFFSET;

            let px = dst_row + x * 4;
            // The +32768 is rounding rather than truncation, worth the one
            // add: truncating biases every channel down by half a code value,
            // which over a whole picture reads as a slightly dark image.
            out[px] = clamp_u8((c + e * v_r + 32_768) >> 16);
            out[px + 1] = clamp_u8((c + e * v_g + d * u_g + 32_768) >> 16);
            out[px + 2] = clamp_u8((c + d * u_b + 32_768) >> 16);
            out[px + 3] = 255;
        }
    }

    true
}

/// Saturate to a byte. Out-of-gamut values are normal in limited-range video,
/// not an error.
fn clamp_u8(value: i32) -> u8 {
    value.clamp(0, 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a tightly packed NV12 buffer of a single flat color.
    fn flat_nv12(width: u32, height: u32, y: u8, u: u8, v: u8) -> (Vec<u8>, PlaneLayout) {
        let (w, h) = (width as usize, height as usize);
        let mut buf = vec![y; w * h];
        for _ in 0..(w * h / 4) {
            buf.push(u);
            buf.push(v);
        }
        let layout = PlaneLayout {
            width,
            height,
            stride: width,
            slice_height: height,
            crop_x: 0,
            crop_y: 0,
            layout: Layout::Nv12,
            color_space: ColorSpace::Bt709,
        };
        (buf, layout)
    }

    /// The RGBA pixel at (x, y).
    fn pixel(out: &[u8], layout: &PlaneLayout, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * layout.width + x) * 4) as usize;
        [out[i], out[i + 1], out[i + 2], out[i + 3]]
    }

    #[test]
    fn limited_range_black_becomes_black() {
        let (buf, layout) = flat_nv12(4, 4, 16, 128, 128);
        let mut out = Vec::new();
        assert!(to_rgba(&buf, &layout, &mut out));
        assert_eq!(pixel(&out, &layout, 0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn limited_range_white_becomes_white() {
        // 235, not 255: studio range. If Y_SCALE were dropped this would come
        // out at about 235 and the picture would look washed out.
        let (buf, layout) = flat_nv12(4, 4, 235, 128, 128);
        let mut out = Vec::new();
        assert!(to_rgba(&buf, &layout, &mut out));
        assert_eq!(pixel(&out, &layout, 0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn full_range_input_saturates_rather_than_wrapping() {
        let (buf, layout) = flat_nv12(4, 4, 255, 128, 128);
        let mut out = Vec::new();
        assert!(to_rgba(&buf, &layout, &mut out));
        // Above white clamps rather than wrapping round to black.
        assert_eq!(pixel(&out, &layout, 0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn alpha_is_always_opaque() {
        let (buf, layout) = flat_nv12(2, 2, 100, 90, 200);
        let mut out = Vec::new();
        assert!(to_rgba(&buf, &layout, &mut out));
        for x in 0..2 {
            for y in 0..2 {
                assert_eq!(pixel(&out, &layout, x, y)[3], 255);
            }
        }
    }

    #[test]
    fn chroma_moves_red_and_blue_in_the_right_directions() {
        let mut out = Vec::new();
        // V above neutral is the red-difference channel.
        let (buf, layout) = flat_nv12(2, 2, 128, 128, 240);
        assert!(to_rgba(&buf, &layout, &mut out));
        let red = pixel(&out, &layout, 0, 0);
        assert!(red[0] > red[2], "high V must give red, got {red:?}");

        // U above neutral is the blue-difference channel.
        let (buf, layout) = flat_nv12(2, 2, 128, 240, 128);
        assert!(to_rgba(&buf, &layout, &mut out));
        let blue = pixel(&out, &layout, 0, 0);
        assert!(blue[2] > blue[0], "high U must give blue, got {blue:?}");
    }

    #[test]
    fn nv21_is_nv12_with_the_chroma_pair_swapped() {
        let mut nv12_out = Vec::new();
        let (buf, layout) = flat_nv12(2, 2, 128, 90, 200);
        assert!(to_rgba(&buf, &layout, &mut nv12_out));

        // The same bytes read as NV21 should give what NV12 gives for the
        // swapped pair, and nothing else.
        let (swapped, _) = flat_nv12(2, 2, 128, 200, 90);
        let mut nv21_out = Vec::new();
        let nv21 = PlaneLayout {
            layout: Layout::Nv21,
            ..layout
        };
        assert!(to_rgba(&swapped, &nv21, &mut nv21_out));
        assert_eq!(nv12_out, nv21_out);
    }

    #[test]
    fn stride_padding_does_not_shear_the_picture() {
        // A 2x2 picture in a buffer whose rows are 8 bytes wide. Read as
        // tightly packed, the second row would start in the padding and the
        // picture would skew.
        let stride = 8u32;
        let mut buf = vec![0u8; (stride * 2) as usize];
        buf[0] = 235; // (0,0) white
        buf[1] = 16; // (1,0) black
        buf[stride as usize] = 16; // (0,1) black
        buf[stride as usize + 1] = 235; // (1,1) white
        buf.extend(std::iter::repeat_n(128, stride as usize));

        let layout = PlaneLayout {
            width: 2,
            height: 2,
            stride,
            slice_height: 2,
            crop_x: 0,
            crop_y: 0,
            layout: Layout::Nv12,
            color_space: ColorSpace::Bt709,
        };
        let mut out = Vec::new();
        assert!(to_rgba(&buf, &layout, &mut out));
        assert_eq!(pixel(&out, &layout, 0, 0)[0], 255);
        assert_eq!(pixel(&out, &layout, 1, 0)[0], 0);
        assert_eq!(pixel(&out, &layout, 0, 1)[0], 0);
        assert_eq!(pixel(&out, &layout, 1, 1)[0], 255);
    }

    #[test]
    fn slice_height_places_the_chroma_planes() {
        // Coded 2x4 with only the top 2 rows visible: chroma starts at row 4,
        // not row 2. Reading it at row 2 would pick up luma as chroma.
        let (w, slice) = (2usize, 4usize);
        let mut buf = vec![128u8; w * slice];
        // Distinctive bytes where a wrong offset would land.
        buf[w * 2] = 0;
        buf[w * 2 + 1] = 0;
        // The real chroma, strongly blue.
        buf.extend_from_slice(&[240, 128, 240, 128]);

        let layout = PlaneLayout {
            width: 2,
            height: 2,
            stride: w as u32,
            slice_height: slice as u32,
            crop_x: 0,
            crop_y: 0,
            layout: Layout::Nv12,
            color_space: ColorSpace::Bt709,
        };
        let mut out = Vec::new();
        assert!(to_rgba(&buf, &layout, &mut out));
        let px = pixel(&out, &layout, 0, 0);
        assert!(px[2] > px[0], "chroma read from the wrong offset: {px:?}");
    }

    #[test]
    fn i420_reads_the_v_plane_after_the_whole_u_plane() {
        // 2x2: 4 luma, then 1 U, then 1 V.
        let mut buf = vec![128u8; 4];
        buf.push(128); // U neutral
        buf.push(240); // V high, so red
        let layout = PlaneLayout {
            width: 2,
            height: 2,
            stride: 2,
            slice_height: 2,
            crop_x: 0,
            crop_y: 0,
            layout: Layout::I420,
            color_space: ColorSpace::Bt709,
        };
        let mut out = Vec::new();
        assert!(to_rgba(&buf, &layout, &mut out));
        let px = pixel(&out, &layout, 0, 0);
        assert!(px[0] > px[2], "U and V planes swapped: {px:?}");
    }

    #[test]
    fn cropping_selects_the_visible_region() {
        // Coded 4x2, visible 2x2 starting at x=2.
        let stride = 4u32;
        let mut buf = vec![16u8; (stride * 2) as usize];
        buf[2] = 235;
        buf[3] = 235;
        buf.extend(std::iter::repeat_n(128, stride as usize));

        let layout = PlaneLayout {
            width: 2,
            height: 2,
            stride,
            slice_height: 2,
            crop_x: 2,
            crop_y: 0,
            layout: Layout::Nv12,
            color_space: ColorSpace::Bt709,
        };
        let mut out = Vec::new();
        assert!(to_rgba(&buf, &layout, &mut out));
        assert_eq!(pixel(&out, &layout, 0, 0)[0], 255, "crop_x was ignored");
        assert_eq!(pixel(&out, &layout, 1, 0)[0], 255);
    }

    #[test]
    fn a_short_buffer_is_refused_rather_than_read_out_of_bounds() {
        let (_, layout) = flat_nv12(64, 64, 16, 128, 128);
        let mut out = Vec::new();
        assert!(!to_rgba(&[0u8; 16], &layout, &mut out));
        assert!(
            out.is_empty(),
            "a refused frame must not leave a half picture"
        );
    }

    #[test]
    fn zero_sized_pictures_are_refused() {
        let (buf, mut layout) = flat_nv12(4, 4, 16, 128, 128);
        layout.width = 0;
        let mut out = Vec::new();
        assert!(!to_rgba(&buf, &layout, &mut out));
    }

    #[test]
    fn the_output_is_tightly_packed_rgba() {
        let (buf, layout) = flat_nv12(8, 4, 128, 128, 128);
        let mut out = Vec::new();
        assert!(to_rgba(&buf, &layout, &mut out));
        assert_eq!(out.len(), 8 * 4 * 4, "egui has nowhere to put a stride");
    }

    #[test]
    fn hd_uses_bt709_and_sd_uses_bt601() {
        assert_eq!(ColorSpace::for_height(1080), ColorSpace::Bt709);
        assert_eq!(ColorSpace::for_height(720), ColorSpace::Bt709);
        assert_eq!(ColorSpace::for_height(480), ColorSpace::Bt601);
    }

    #[test]
    fn the_two_matrices_differ_on_the_same_pixel() {
        // Not a value check, a wiring check: if the color space were ignored,
        // these would be identical.
        let mut a = Vec::new();
        let mut b = Vec::new();
        let (buf, layout) = flat_nv12(2, 2, 150, 100, 180);
        assert!(to_rgba(&buf, &layout, &mut a));
        let sd = PlaneLayout {
            color_space: ColorSpace::Bt601,
            ..layout
        };
        assert!(to_rgba(&buf, &sd, &mut b));
        assert_ne!(a, b);
    }
}
