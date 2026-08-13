//! PES header parsing, for the presentation timestamps that anchor everything
//! downstream: cut points, chapter positions and audio/video sync.

/// The 90 kHz clock PTS and DTS are expressed in.
pub const PTS_CLOCK_HZ: u64 = 90_000;

/// PTS and DTS are 33-bit and wrap roughly every 26.5 hours.
pub const PTS_MODULO: u64 = 1 << 33;

/// The parts of a PES header Asaborake acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PesHeader {
    /// Which elementary stream this packet belongs to.
    pub stream_id: u8,
    /// Presentation timestamp in 90 kHz units, when signalled.
    pub pts: Option<u64>,
    /// Decode timestamp in 90 kHz units, when signalled.
    pub dts: Option<u64>,
    /// Offset of the elementary-stream bytes within the PES packet.
    pub payload_offset: usize,
}

impl PesHeader {
    /// Parse the header at the start of a PES packet.
    ///
    /// Returns `None` for anything that is not a PES packet, including the
    /// padding and private streams that carry no timestamps worth having.
    #[must_use]
    pub fn parse(buf: &[u8]) -> Option<Self> {
        // packet_start_code_prefix is a fixed 0x000001.
        if buf.len() < 9 || buf[0] != 0x00 || buf[1] != 0x00 || buf[2] != 0x01 {
            return None;
        }
        let stream_id = buf[3];

        // Padding, and a handful of system streams, have no optional header.
        if matches!(stream_id, 0xBC | 0xBE | 0xBF | 0xF0..=0xF2 | 0xF8 | 0xFF) {
            return Some(Self {
                stream_id,
                pts: None,
                dts: None,
                payload_offset: 6,
            });
        }

        let flags = buf[7];
        let header_len = usize::from(buf[8]);
        let payload_offset = 9 + header_len;
        if payload_offset > buf.len() {
            return None;
        }

        // The two flags are only meaningful together: DTS without PTS is not a
        // legal combination, and streams that signal it are treated as having
        // neither rather than trusting a malformed header.
        let presentation_signalled = flags & 0x80 != 0;
        let decode_signalled = flags & 0x40 != 0;

        let pts = if presentation_signalled {
            buf.get(9..14).map(read_timestamp)
        } else {
            None
        };
        let dts = if presentation_signalled && decode_signalled {
            buf.get(14..19).map(read_timestamp)
        } else {
            None
        };

        Some(Self {
            stream_id,
            pts,
            dts,
            payload_offset,
        })
    }
}

/// Decode the 5-byte marker-interleaved 33-bit timestamp encoding.
fn read_timestamp(b: &[u8]) -> u64 {
    (u64::from(b[0] & 0x0E) << 29)
        | (u64::from(b[1]) << 22)
        | (u64::from(b[2] & 0xFE) << 14)
        | (u64::from(b[3]) << 7)
        | (u64::from(b[4]) >> 1)
}

/// Accumulates 33-bit timestamps into a monotonic timeline.
///
/// A recording longer than ~26.5 hours wraps, and broadcast occasionally
/// restarts the timebase outright. Both look like a huge backwards jump, so
/// they are handled the same way: unwrap small backwards steps, and rebase on
/// anything too large to be a wrap.
#[derive(Debug, Default)]
pub struct PtsUnwrapper {
    last: Option<i64>,
    /// Accumulated `PTS_MODULO` steps added back after each wrap.
    offset: i64,
    /// Accumulated correction applied when the timebase was reset outright.
    rebase: i64,
}

/// A 33-bit timestamp masked into range, as a signed tick count.
///
/// Masking makes the conversion total: the result is below 2^33, so it always
/// fits in `i64` and the whole unwrapper can work in signed arithmetic without
/// a single lossy cast.
fn masked(pts: u64) -> i64 {
    i64::try_from(pts & (PTS_MODULO - 1)).unwrap_or(0)
}

impl PtsUnwrapper {
    /// Create a fresh unwrapper.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a raw 33-bit timestamp; returns it on a continuous timeline.
    pub fn push(&mut self, pts: u64) -> i64 {
        let pts = masked(pts);
        let Some(last) = self.last else {
            self.last = Some(pts);
            return pts;
        };

        // Half the modulo is the classic wrap threshold: a step further
        // backwards than that is more plausibly a wrap than a real rewind.
        let half = MODULO_TICKS / 2;
        let delta = pts - last;

        if delta < -half {
            self.offset += MODULO_TICKS;
        } else if delta > half || delta < -RESTART_TICKS {
            // The timebase restarted. That happens forwards when an encoder
            // resets its clock, and backwards whenever two separately encoded
            // stretches end up in one file — which is what a recording of
            // consecutive broadcast material is, and what happens across a
            // long signal dropout.
            //
            // Either way the jump is not elapsed time, so it is cancelled out
            // and the timeline continues from where it had reached. Without
            // this a 150-second recording made of five segments reports the
            // length of one of them.
            self.rebase -= delta;
        }

        self.last = Some(pts);
        pts + self.offset + self.rebase
    }

    /// Convert an unwrapped 90 kHz timestamp to seconds.
    #[must_use]
    pub fn to_seconds(ticks: i64) -> f64 {
        ticks as f64 / PTS_CLOCK_HZ as f64
    }
}

/// [`PTS_MODULO`] as signed ticks, for the unwrapper's arithmetic.
const MODULO_TICKS: i64 = 1 << 33;

/// A backwards step larger than this is a timebase restart, not reordering.
///
/// Timestamps are read in decode order, so B-frames make small backwards steps
/// entirely normal — a couple of frames' worth. Ten seconds is far beyond any
/// reordering window and far below any plausible content.
const RESTART_TICKS: i64 = 10 * PTS_CLOCK_HZ.cast_signed();

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_timestamp(prefix: u8, value: u64) -> [u8; 5] {
        [
            (prefix << 4) | (((value >> 29) as u8) & 0x0E) | 0x01,
            ((value >> 22) & 0xFF) as u8,
            ((((value >> 14) as u8) & 0xFE) | 0x01),
            ((value >> 7) & 0xFF) as u8,
            ((((value << 1) as u8) & 0xFE) | 0x01),
        ]
    }

    #[test]
    fn parses_pts_only_header() {
        let pts = 123_456_789u64;
        let mut buf = vec![0x00, 0x00, 0x01, 0xE0, 0x00, 0x00, 0x80, 0x80, 5];
        buf.extend_from_slice(&encode_timestamp(0b0010, pts));
        buf.extend_from_slice(&[0xAA, 0xBB]);

        let header = PesHeader::parse(&buf).expect("pes header");
        assert_eq!(header.stream_id, 0xE0);
        assert_eq!(header.pts, Some(pts));
        assert_eq!(header.dts, None);
        assert_eq!(header.payload_offset, 14);
    }

    #[test]
    fn parses_pts_and_dts_header() {
        let (pts, dts) = (900_000u64, 897_000u64);
        let mut buf = vec![0x00, 0x00, 0x01, 0xE0, 0x00, 0x00, 0x80, 0xC0, 10];
        buf.extend_from_slice(&encode_timestamp(0b0011, pts));
        buf.extend_from_slice(&encode_timestamp(0b0001, dts));

        let header = PesHeader::parse(&buf).expect("pes header");
        assert_eq!(header.pts, Some(pts));
        assert_eq!(header.dts, Some(dts));
    }

    #[test]
    fn rejects_non_pes_payload() {
        assert!(PesHeader::parse(&[0xFF; 16]).is_none());
    }

    #[test]
    fn unwraps_across_the_33_bit_boundary() {
        let mut unwrapper = PtsUnwrapper::new();
        let before = PTS_MODULO - 90_000;
        let before_ticks = MODULO_TICKS - 90_000;
        assert_eq!(unwrapper.push(before), before_ticks);
        // One second later, having wrapped through zero.
        let after = unwrapper.push(0);
        assert_eq!(after, MODULO_TICKS);
        assert!((PtsUnwrapper::to_seconds(after - before_ticks) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_concatenated_recording_keeps_accumulating_time() {
        // Five separately encoded 30-second stretches in one file, each
        // starting its clock near zero — which is what a recording of
        // consecutive broadcast material looks like.
        let mut unwrapper = PtsUnwrapper::new();
        let thirty_seconds = 30 * PTS_CLOCK_HZ;

        let mut last = 0;
        for _segment in 0..5 {
            for tick in 0..=30 {
                last = unwrapper.push(tick * PTS_CLOCK_HZ);
            }
            // The next segment restarts near zero.
            let _ = thirty_seconds;
        }

        let total = PtsUnwrapper::to_seconds(last);
        assert!(
            (total - 150.0).abs() < 1.0,
            "five 30-second segments should total 150s, got {total}s"
        );
    }

    #[test]
    fn small_backwards_steps_are_frame_reordering_not_a_restart() {
        // Timestamps arrive in decode order, so B-frames legitimately step
        // backwards by a frame or two.
        let mut unwrapper = PtsUnwrapper::new();
        let frame = PTS_CLOCK_HZ / 30;
        let base = 90_000;

        let a = unwrapper.push(base + 3 * frame);
        let b = unwrapper.push(base + frame);
        let c = unwrapper.push(base + 2 * frame);

        assert!(b < a, "reordering must be preserved, not cancelled");
        assert_eq!(
            c - b,
            i64::try_from(frame).unwrap_or(0),
            "and the timeline must stay sane"
        );
    }

    #[test]
    fn rebases_when_the_encoder_clock_restarts() {
        let mut unwrapper = PtsUnwrapper::new();
        let start = 1_000_000u64;
        let a = unwrapper.push(start);
        let b = unwrapper.push(start + 90_000);
        assert_eq!(b - a, 90_000);

        // A jump forwards of many hours is a timebase reset, not real elapsed
        // time; the timeline must not gain those hours.
        let c = unwrapper.push(start + 90_000 + PTS_MODULO / 2 + 90_000);
        assert!(c - b < 90_000 * 10, "reset should not add hours: {}", c - b);
    }
}
