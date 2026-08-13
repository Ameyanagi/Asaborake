//! PSI section reassembly and the two tables Asaborake needs: PAT and PMT.
//!
//! Sections are spread across packets and prefixed by a pointer field, so they
//! have to be reassembled before they can be parsed. Broadcast repeats both
//! tables continuously, which is what lets a recording that started mid-stream
//! still be understood.

use crate::packet::TsPacket;

/// Table id of the Program Association Table.
const TABLE_ID_PAT: u8 = 0x00;
/// Table id of the Program Map Table.
const TABLE_ID_PMT: u8 = 0x02;

/// Reassembles PSI sections arriving on one PID.
#[derive(Debug, Default)]
pub struct SectionAssembler {
    buffer: Vec<u8>,
    /// Total section length expected, derived from the section header.
    expected: usize,
}

impl SectionAssembler {
    /// Create an empty assembler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one packet's payload; returns each section that completed.
    ///
    /// A packet may both finish the previous section and start a new one, so
    /// this returns a list rather than an `Option`.
    pub fn push(&mut self, packet: &TsPacket<'_>) -> Vec<Vec<u8>> {
        let mut done = Vec::new();
        if !packet.has_payload() || packet.is_scrambled() {
            return done;
        }
        let payload = packet.payload;

        let body = if packet.payload_unit_start {
            // The first byte is a pointer to where the next section starts;
            // anything before it belongs to the section still in flight.
            let pointer = usize::from(payload[0]);
            if 1 + pointer > payload.len() {
                self.reset();
                return done;
            }
            let (tail, rest) = payload[1..].split_at(pointer);
            if !self.buffer.is_empty() {
                self.buffer.extend_from_slice(tail);
                if let Some(section) = self.take_if_complete() {
                    done.push(section);
                }
            }
            self.reset();
            rest
        } else {
            if self.buffer.is_empty() {
                // Mid-section bytes with no header seen yet: nothing to attach
                // them to, which is normal at the very start of a recording.
                return done;
            }
            payload
        };

        self.buffer.extend_from_slice(body);
        if self.expected == 0 && self.buffer.len() >= 3 {
            // section_length counts the bytes after the 3-byte header.
            let length = ((usize::from(self.buffer[1]) & 0x0F) << 8) | usize::from(self.buffer[2]);
            self.expected = length + 3;
        }
        while let Some(section) = self.take_if_complete() {
            done.push(section);
            if self.buffer.len() >= 3 && self.buffer[0] != 0xFF {
                let length =
                    ((usize::from(self.buffer[1]) & 0x0F) << 8) | usize::from(self.buffer[2]);
                self.expected = length + 3;
            } else {
                self.reset();
                break;
            }
        }
        done
    }

    fn take_if_complete(&mut self) -> Option<Vec<u8>> {
        if self.expected == 0 || self.buffer.len() < self.expected {
            return None;
        }
        let section: Vec<u8> = self.buffer.drain(..self.expected).collect();
        self.expected = 0;
        Some(section)
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.expected = 0;
    }
}

/// Program Association Table: maps program numbers to their PMT PIDs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pat {
    /// `(program_number, pmt_pid)` pairs. Program number 0 is the NIT and is
    /// filtered out here because it carries no elementary streams.
    pub programs: Vec<(u16, u16)>,
}

impl Pat {
    /// Parse a complete PAT section.
    #[must_use]
    pub fn parse(section: &[u8]) -> Option<Self> {
        if section.first()? != &TABLE_ID_PAT {
            return None;
        }
        let body = section_body(section)?;
        let mut programs = Vec::new();
        for entry in body.chunks_exact(4) {
            let program_number = (u16::from(entry[0]) << 8) | u16::from(entry[1]);
            let pid = (u16::from(entry[2] & 0x1F) << 8) | u16::from(entry[3]);
            if program_number != 0 {
                programs.push((program_number, pid));
            }
        }
        Some(Self { programs })
    }
}

/// One elementary stream listed in a PMT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EsInfo {
    /// PID the elementary stream is carried on.
    pub pid: u16,
    /// Stream type byte, see [`StreamKind`].
    pub stream_type: u8,
    /// Value of the ARIB stream identifier descriptor (0x52), when present.
    ///
    /// Japanese broadcast relies on this rather than stream type alone to tell
    /// captions (0x30–0x37) from superimpose (0x38–0x3F).
    pub component_tag: Option<u8>,
}

/// Program Map Table: the elementary streams making up one program.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pmt {
    /// PID carrying the program clock reference.
    pub pcr_pid: u16,
    /// Elementary streams in the order the table lists them.
    pub streams: Vec<EsInfo>,
}

impl Pmt {
    /// Parse a complete PMT section.
    #[must_use]
    pub fn parse(section: &[u8]) -> Option<Self> {
        if section.first()? != &TABLE_ID_PMT {
            return None;
        }
        let body = section_body(section)?;
        if body.len() < 4 {
            return None;
        }
        let pcr_pid = (u16::from(body[0] & 0x1F) << 8) | u16::from(body[1]);
        let program_info_len = ((usize::from(body[2]) & 0x0F) << 8) | usize::from(body[3]);
        let mut cursor = 4 + program_info_len;

        let mut streams = Vec::new();
        while cursor + 5 <= body.len() {
            let stream_type = body[cursor];
            let pid = (u16::from(body[cursor + 1] & 0x1F) << 8) | u16::from(body[cursor + 2]);
            let es_info_len =
                ((usize::from(body[cursor + 3]) & 0x0F) << 8) | usize::from(body[cursor + 4]);
            let desc_start = cursor + 5;
            let desc_end = desc_start + es_info_len;
            if desc_end > body.len() {
                break;
            }
            streams.push(EsInfo {
                pid,
                stream_type,
                component_tag: find_component_tag(&body[desc_start..desc_end]),
            });
            cursor = desc_end;
        }
        Some(Self { pcr_pid, streams })
    }
}

/// Strip the 8-byte section header and the 4-byte CRC, yielding the table body.
fn section_body(section: &[u8]) -> Option<&[u8]> {
    // 3-byte generic header, 5 bytes of long-form syntax, 4-byte trailing CRC.
    const HEADER: usize = 8;
    const CRC: usize = 4;
    if section.len() < HEADER + CRC {
        return None;
    }
    section.get(HEADER..section.len() - CRC)
}

/// Scan a descriptor loop for the ARIB stream identifier descriptor (tag 0x52).
fn find_component_tag(mut descriptors: &[u8]) -> Option<u8> {
    while descriptors.len() >= 2 {
        let tag = descriptors[0];
        let len = usize::from(descriptors[1]);
        let body = descriptors.get(2..2 + len)?;
        if tag == 0x52 {
            return body.first().copied();
        }
        descriptors = &descriptors[2 + len..];
    }
    None
}

/// What an elementary stream carries, resolved from its stream type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    /// MPEG-2 video, still the norm for Japanese terrestrial broadcast.
    Mpeg2Video,
    /// H.264/AVC video, used by some satellite services.
    H264Video,
    /// H.265/HEVC video, used by 4K services.
    HevcVideo,
    /// AAC audio in ADTS framing.
    AacAudio,
    /// ARIB B24 closed captions.
    Caption,
    /// ARIB B24 superimpose (emergency crawls and similar overlays).
    Superimpose,
    /// Data carousel and other non-media payloads.
    Data,
    /// Anything Asaborake does not act on.
    Other(u8),
}

impl StreamKind {
    /// Resolve a stream type, using the component tag to disambiguate the
    /// private-data type that ARIB uses for both captions and superimpose.
    #[must_use]
    pub const fn resolve(stream_type: u8, component_tag: Option<u8>) -> Self {
        match stream_type {
            0x02 => Self::Mpeg2Video,
            0x1B => Self::H264Video,
            0x24 => Self::HevcVideo,
            0x0F | 0x11 => Self::AacAudio,
            0x06 => match component_tag {
                Some(0x30..=0x37) => Self::Caption,
                Some(0x38..=0x3F) => Self::Superimpose,
                _ => Self::Data,
            },
            0x0D => Self::Data,
            other => Self::Other(other),
        }
    }

    /// Whether this stream carries pictures.
    #[must_use]
    pub const fn is_video(self) -> bool {
        matches!(self, Self::Mpeg2Video | Self::H264Video | Self::HevcVideo)
    }

    /// Whether this stream carries sound.
    #[must_use]
    pub const fn is_audio(self) -> bool {
        matches!(self, Self::AacAudio)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::SYNC_BYTE;

    /// Wrap a table body in a long-form section, with a placeholder CRC.
    fn section(table_id: u8, body: &[u8]) -> Vec<u8> {
        let mut s = vec![table_id, 0, 0];
        // 5 bytes of long-form syntax, then the body, then 4 CRC bytes.
        s.extend_from_slice(&[0x00, 0x01, 0xC1, 0x00, 0x00]);
        s.extend_from_slice(body);
        s.extend_from_slice(&[0, 0, 0, 0]);
        let length = s.len() - 3;
        s[1] = 0xB0 | u8::try_from(length >> 8).expect("section length fits");
        s[2] = u8::try_from(length & 0xFF).expect("low byte");
        s
    }

    fn packet_carrying(pid: u16, start: bool, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0xFFu8; 188];
        p[0] = SYNC_BYTE;
        p[1] = (u8::try_from(pid >> 8).expect("pid high")) | if start { 0x40 } else { 0x00 };
        p[2] = (pid & 0xFF) as u8;
        p[3] = 0x10;
        let mut cursor = 4;
        if start {
            p[cursor] = 0; // pointer field
            cursor += 1;
        }
        assert!(
            payload.len() <= 188 - cursor,
            "payload must fit in one packet"
        );
        p[cursor..cursor + payload.len()].copy_from_slice(payload);
        p
    }

    /// Bytes of section payload one packet can carry.
    const fn capacity(start: bool) -> usize {
        if start { 183 } else { 184 }
    }

    #[test]
    fn parses_pat_dropping_the_nit_entry() {
        let body = [
            0x00, 0x00, 0xE0, 0x10, // program 0 (NIT) -> pid 0x0010
            0x04, 0x00, 0xE1, 0x00, // program 1024 -> pid 0x0100
        ];
        let pat = Pat::parse(&section(TABLE_ID_PAT, &body)).expect("pat");
        assert_eq!(pat.programs, vec![(0x0400, 0x0100)]);
    }

    #[test]
    fn parses_pmt_with_descriptors() {
        let body = [
            0xE1, 0x00, // pcr pid 0x0100
            0x00, 0x00, // no program info
            0x02, 0xE1, 0x00, 0x00, 0x00, // mpeg2 video on 0x0100
            0x0F, 0xE1, 0x10, 0x00, 0x00, // aac on 0x0110
            0x06, 0xE1, 0x20, 0x00, 0x03, 0x52, 0x01, 0x30, // caption on 0x0120
        ];
        let pmt = Pmt::parse(&section(TABLE_ID_PMT, &body)).expect("pmt");
        assert_eq!(pmt.pcr_pid, 0x0100);
        assert_eq!(pmt.streams.len(), 3);
        assert_eq!(pmt.streams[2].component_tag, Some(0x30));
        assert_eq!(
            StreamKind::resolve(pmt.streams[2].stream_type, pmt.streams[2].component_tag),
            StreamKind::Caption
        );
        assert!(StreamKind::resolve(pmt.streams[0].stream_type, None).is_video());
        assert!(StreamKind::resolve(pmt.streams[1].stream_type, None).is_audio());
    }

    #[test]
    fn reassembles_a_section_split_across_packets() {
        // A PAT listing 60 services is larger than one packet can carry, which
        // is the case that forces reassembly. Full-transponder captures of a
        // BS multiplex produce exactly this.
        let body: Vec<u8> = (0u16..60)
            .flat_map(|i| {
                let program = i + 1;
                let pid = 0x0100 + i;
                [
                    (program >> 8) as u8,
                    (program & 0xFF) as u8,
                    0xE0 | ((pid >> 8) as u8),
                    (pid & 0xFF) as u8,
                ]
            })
            .collect();
        let sec = section(TABLE_ID_PAT, &body);
        assert!(sec.len() > capacity(true), "section must span two packets");

        let mut assembler = SectionAssembler::new();

        let first = packet_carrying(0, true, &sec[..capacity(true)]);
        let done = assembler.push(&TsPacket::parse(&first).expect("packet"));
        assert!(done.is_empty(), "section is not complete yet");

        let second = packet_carrying(0, false, &sec[capacity(true)..]);
        let done = assembler.push(&TsPacket::parse(&second).expect("packet"));
        assert_eq!(done.len(), 1, "the second packet completes the section");

        let pat = Pat::parse(&done[0]).expect("pat");
        assert_eq!(pat.programs.len(), 60);
        assert_eq!(pat.programs[0], (1, 0x0100));
        assert_eq!(pat.programs[59], (60, 0x013B));
    }

    #[test]
    fn superimpose_is_distinguished_from_caption() {
        assert_eq!(
            StreamKind::resolve(0x06, Some(0x38)),
            StreamKind::Superimpose
        );
        assert_eq!(StreamKind::resolve(0x06, Some(0x30)), StreamKind::Caption);
        assert_eq!(StreamKind::resolve(0x06, None), StreamKind::Data);
    }
}
