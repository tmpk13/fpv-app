//! The pages, one file each.
//!
//! Every file here is a page's definition: what is on it, in what order, bound
//! to what state. The shared vocabulary they are written in lives beside them
//! in [`super::widgets`] and [`super::theme`], the prose in [`super::text`],
//! and the one piece of drawing big enough to be its own thing - the link
//! history graph - in [`super::plot`].

mod link;
mod settings;
mod video;

/// A bit rate as a short human-readable string.
///
/// Rounded to three significant figures at most: an FPV bitrate that jitters
/// between 4.213 and 4.219 Mbit/s is one reading, and showing every digit of
/// it makes a steady link look unsteady.
pub(super) fn bitrate(bits_per_second: f64) -> String {
    if bits_per_second >= 1_000_000.0 {
        format!("{:.2} Mbit/s", bits_per_second / 1_000_000.0)
    } else if bits_per_second >= 1_000.0 {
        format!("{:.0} kbit/s", bits_per_second / 1_000.0)
    } else {
        format!("{bits_per_second:.0} bit/s")
    }
}

/// A count with thousands separators, because these run to seven digits on a
/// long flight and are unreadable without.
pub(super) fn count(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// A "seconds since" reading, or a dash when it has never happened.
pub(super) fn since(seconds: Option<f64>) -> String {
    match seconds {
        None => "-".to_string(),
        Some(s) if s < 60.0 => format!("{s:.1} s"),
        Some(s) => format!("{:.0} min", s / 60.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitrates_use_a_sensible_unit() {
        assert_eq!(bitrate(0.0), "0 bit/s");
        assert_eq!(bitrate(900.0), "900 bit/s");
        assert_eq!(bitrate(12_000.0), "12 kbit/s");
        assert_eq!(bitrate(4_200_000.0), "4.20 Mbit/s");
    }

    #[test]
    fn counts_are_grouped_in_threes() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1_000), "1 000");
        assert_eq!(count(1_234_567), "1 234 567");
    }

    #[test]
    fn a_thing_that_never_happened_reads_as_a_dash() {
        assert_eq!(since(None), "-");
        assert_eq!(since(Some(0.4)), "0.4 s");
        assert_eq!(since(Some(125.0)), "2 min");
    }
}
