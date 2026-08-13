//! Transport-stream packet framing.
//!
//! Japanese recorders emit three layouts in practice: bare 188-byte packets,
//! 192-byte packets carrying a 4-byte arrival timestamp in front (the "M2TS"
//! layout some capture cards and Blu-ray tools use), and 204-byte packets with
//! a Reed-Solomon parity tail. All three are handled by locating the sync byte
//! at a consistent stride rather than trusting the file extension.

use crate::Error;

/// Transport-stream sync byte, present at the head of every packet.
pub const SYNC_BYTE: u8 = 0x47;

/// PID reserved for the Program Association Table.
pub const PID_PAT: u16 = 0x0000;
/// PID reserved for the Conditional Access Table.
pub const PID_CAT: u16 = 0x0001;
/// Null packets used purely as stuffing; always discarded.
pub const PID_NULL: u16 = 0x1FFF;

/// The on-disk stride of one packet, including any prefix or parity bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PacketLayout {
    /// Bare 188-byte packets.
    Ts188,
    /// 4-byte arrival timestamp followed by a 188-byte packet.
    M2ts192,
    /// 188-byte packet followed by 16 bytes of Reed-Solomon parity.
    Rs204,
}

impl PacketLayout {
    /// Bytes from the start of one packet to the start of the next.
    #[must_use]
    pub const fn stride(self) -> usize {
        match self {
            Self::Ts188 => 188,
            Self::M2ts192 => 192,
            Self::Rs204 => 204,
        }
    }

    /// Offset of the sync byte within one stride.
    #[must_use]
    pub const fn sync_offset(self) -> usize {
        match self {
            Self::Ts188 | Self::Rs204 => 0,
            Self::M2ts192 => 4,
        }
    }

    /// Every layout this crate knows how to read.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Ts188, Self::M2ts192, Self::Rs204]
    }
}

/// Identify the packet layout, and the offset of the first whole packet, by
/// looking for a sync byte that repeats at a candidate stride.
///
/// A recording may begin mid-packet — a tuner started while a broadcast was
/// already in flight produces exactly that — so the search slides over the
/// first stride's worth of bytes rather than assuming byte zero.
///
/// # Errors
/// Returns [`Error::NoSync`] when no candidate holds for enough consecutive
/// packets, which in practice means the input is not a transport stream.
pub fn detect_layout(buf: &[u8]) -> Result<(PacketLayout, usize), Error> {
    // Eight consecutive hits rules out coincidence: the odds of random data
    // placing 0x47 at eight exact multiples of a stride are about 2^-64. A
    // buffer too short to hold eight packets confirms as many as it can, with
    // a floor of two so a single stray sync byte can never satisfy the check.
    const CONFIRM: usize = 8;
    const MIN_CONFIRM: usize = 2;

    for layout in PacketLayout::all() {
        let stride = layout.stride();
        let sync = layout.sync_offset();
        let limit = stride.min(buf.len());
        for start in 0..limit {
            let available = buf.len().saturating_sub(start + sync) / stride;
            let required = available.clamp(MIN_CONFIRM, CONFIRM);
            if available < required {
                continue;
            }
            let confirmed = (0..required).all(|i| {
                buf.get(start + i * stride + sync)
                    .is_some_and(|&b| b == SYNC_BYTE)
            });
            if confirmed {
                return Ok((layout, start));
            }
        }
    }
    Err(Error::NoSync)
}

/// One parsed 188-byte transport packet, borrowing from the read buffer.
#[derive(Debug, Clone, Copy)]
pub struct TsPacket<'a> {
    /// Packet identifier: which elementary or table stream this belongs to.
    pub pid: u16,
    /// Payload-unit-start indicator: a PES or section header begins here.
    pub payload_unit_start: bool,
    /// Set when the demodulator flagged an uncorrectable error in this packet.
    pub transport_error: bool,
    /// Non-zero when the payload is still encrypted (a CAS failure, for us).
    pub scrambling_control: u8,
    /// Four-bit counter that increments per packet with payload on this PID.
    pub continuity_counter: u8,
    /// Whether the adaptation field signals an intentional timebase break.
    pub discontinuity: bool,
    /// Whether this packet starts a random-access point (a keyframe, in practice).
    pub random_access: bool,
    /// Program clock reference, in 27 MHz units, when this packet carries one.
    pub pcr: Option<u64>,
    /// Payload bytes after any adaptation field, empty when there is none.
    pub payload: &'a [u8],
}

impl<'a> TsPacket<'a> {
    /// Parse a single 188-byte packet.
    ///
    /// Returns `None` when the buffer is not a well-formed packet; callers
    /// treat that as a resync trigger rather than a fatal error, because a
    /// recording with a corrupt run is still worth processing around.
    #[must_use]
    pub fn parse(buf: &'a [u8]) -> Option<Self> {
        if buf.len() < 188 || buf[0] != SYNC_BYTE {
            return None;
        }

        let transport_error = buf[1] & 0x80 != 0;
        let payload_unit_start = buf[1] & 0x40 != 0;
        let pid = (u16::from(buf[1] & 0x1F) << 8) | u16::from(buf[2]);
        let scrambling_control = (buf[3] >> 6) & 0x03;
        let has_adaptation = buf[3] & 0x20 != 0;
        let has_payload = buf[3] & 0x10 != 0;
        let continuity_counter = buf[3] & 0x0F;

        let mut cursor = 4usize;
        let mut pcr = None;
        let mut discontinuity = false;
        let mut random_access = false;

        if has_adaptation {
            let adaptation_len = usize::from(buf[4]);
            cursor = 5 + adaptation_len;
            if cursor > 188 {
                return None;
            }
            if adaptation_len > 0 {
                let flags = buf[5];
                discontinuity = flags & 0x80 != 0;
                random_access = flags & 0x40 != 0;
                if flags & 0x10 != 0 && adaptation_len >= 7 {
                    pcr = Some(read_pcr(&buf[6..12]));
                }
            }
        }

        let payload = if has_payload { &buf[cursor..188] } else { &[] };

        Some(Self {
            pid,
            payload_unit_start,
            transport_error,
            scrambling_control,
            continuity_counter,
            discontinuity,
            random_access,
            pcr,
            payload,
        })
    }

    /// Whether this packet carries any payload bytes.
    #[must_use]
    pub const fn has_payload(&self) -> bool {
        !self.payload.is_empty()
    }

    /// Whether the payload is scrambled, which for a recording means the card
    /// or SoftCAS failed to decrypt this section.
    #[must_use]
    pub const fn is_scrambled(&self) -> bool {
        self.scrambling_control != 0
    }
}

/// Decode a 48-bit PCR field into 27 MHz ticks.
fn read_pcr(b: &[u8]) -> u64 {
    let base = (u64::from(b[0]) << 25)
        | (u64::from(b[1]) << 17)
        | (u64::from(b[2]) << 9)
        | (u64::from(b[3]) << 1)
        | (u64::from(b[4]) >> 7);
    let ext = (u64::from(b[4] & 0x01) << 8) | u64::from(b[5]);
    base * 300 + ext
}

/// Tracks continuity counters per PID so that dropped packets can be counted.
///
/// The counter is four bits and only advances on packets that carry payload,
/// so both duplicates (same value) and stuffing-only packets must be excluded
/// before a gap can be called a drop.
#[derive(Debug, Default)]
pub struct ContinuityTracker {
    last: std::collections::HashMap<u16, u8>,
}

impl ContinuityTracker {
    /// Create an empty tracker.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one packet; returns how many packets appear to have been lost.
    pub fn push(&mut self, packet: &TsPacket<'_>) -> u32 {
        if !packet.has_payload() || packet.pid == PID_NULL {
            return 0;
        }
        let cc = packet.continuity_counter;
        let Some(previous) = self.last.insert(packet.pid, cc) else {
            return 0;
        };
        if packet.discontinuity {
            return 0;
        }
        // A repeated counter is a legal duplicate packet, not a loss.
        if cc == previous {
            return 0;
        }
        let expected = (previous + 1) & 0x0F;
        if cc == expected {
            0
        } else {
            u32::from((cc.wrapping_sub(expected)) & 0x0F)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal packet with the given PID, CC and payload.
    fn packet(pid: u16, cc: u8, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 188];
        p[0] = SYNC_BYTE;
        p[1] = (pid >> 8) as u8;
        p[2] = (pid & 0xFF) as u8;
        p[3] = 0x10 | (cc & 0x0F);
        p[4..4 + payload.len()].copy_from_slice(payload);
        p
    }

    #[test]
    fn parses_pid_and_payload() {
        let raw = packet(0x0100, 5, &[1, 2, 3]);
        let pkt = TsPacket::parse(&raw).expect("well-formed packet");
        assert_eq!(pkt.pid, 0x0100);
        assert_eq!(pkt.continuity_counter, 5);
        assert_eq!(&pkt.payload[..3], &[1, 2, 3]);
        assert!(!pkt.is_scrambled());
    }

    #[test]
    fn rejects_packet_without_sync() {
        let mut raw = packet(0x0100, 0, &[]);
        raw[0] = 0x00;
        assert!(TsPacket::parse(&raw).is_none());
    }

    #[test]
    fn detects_bare_188_layout() {
        let stream: Vec<u8> = (0..10).flat_map(|i| packet(0x0100, i, &[])).collect();
        let (layout, start) = detect_layout(&stream).expect("layout");
        assert_eq!(layout, PacketLayout::Ts188);
        assert_eq!(start, 0);
    }

    #[test]
    fn detects_m2ts_192_layout_and_leading_partial_packet() {
        let mut stream = vec![0xAA; 7];
        for i in 0..10 {
            stream.extend_from_slice(&[0, 0, 0, 0]);
            stream.extend_from_slice(&packet(0x0100, i, &[]));
        }
        let (layout, start) = detect_layout(&stream).expect("layout");
        assert_eq!(layout, PacketLayout::M2ts192);
        assert_eq!(start, 7);
    }

    #[test]
    fn continuity_counts_gap_but_ignores_duplicates() {
        let mut tracker = ContinuityTracker::new();
        for (cc, expected_loss) in [(0, 0), (1, 0), (1, 0), (5, 3), (6, 0)] {
            let raw = packet(0x0100, cc, &[9]);
            let pkt = TsPacket::parse(&raw).expect("packet");
            assert_eq!(tracker.push(&pkt), expected_loss, "cc={cc}");
        }
    }

    #[test]
    fn continuity_ignores_signalled_discontinuity() {
        let mut tracker = ContinuityTracker::new();
        let first = packet(0x0100, 0, &[9]);
        tracker.push(&TsPacket::parse(&first).expect("packet"));

        // Adaptation field present, discontinuity_indicator set.
        let mut raw = vec![0u8; 188];
        raw[0] = SYNC_BYTE;
        raw[1] = 0x01;
        raw[2] = 0x00;
        raw[3] = 0x30 | 0x08; // adaptation + payload, cc=8
        raw[4] = 1;
        raw[5] = 0x80;
        let pkt = TsPacket::parse(&raw).expect("packet");
        assert!(pkt.discontinuity);
        assert_eq!(tracker.push(&pkt), 0);
    }

    #[test]
    fn reads_pcr_from_adaptation_field() {
        let mut raw = vec![0u8; 188];
        raw[0] = SYNC_BYTE;
        raw[1] = 0x01;
        raw[2] = 0x00;
        raw[3] = 0x30;
        raw[4] = 7;
        raw[5] = 0x10;
        // base = 1, ext = 0
        raw[6..12].copy_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x80, 0x00]);
        let pkt = TsPacket::parse(&raw).expect("packet");
        assert_eq!(pkt.pcr, Some(300));
    }
}
