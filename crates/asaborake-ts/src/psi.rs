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
/// Table id of this multiplex's present/following Event Information Table.
const TABLE_ID_EIT_PF: u8 = 0x4E;

/// PID carrying the Event Information Table.
pub const PID_EIT: u16 = 0x0012;

/// Table id of the Service Description Table for this transport stream.
const TABLE_ID_SDT: u8 = 0x42;

/// PID carrying the Service Description Table.
pub const PID_SDT: u16 = 0x0011;

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

/// What the ARIB audio component descriptor says about an audio stream.
///
/// This is the only place a recording states that it is bilingual. A dual-mono
/// stream looks exactly like stereo to a decoder — two channels, one AAC
/// stream — so without reading this, a bilingual programme is transcoded with
/// both languages talking over each other in the same stereo pair.
///
/// It appears in the *event* information table rather than the program map:
/// what languages a programme carries is a property of the programme, not of
/// the multiplex, and it changes when the programme does.
///
/// Defined in ARIB STD-B10 part 2, 6.2.26.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AudioComponent {
    /// Channel arrangement: `0x02` is dual mono, `0x03` stereo, `0x01` mono.
    pub component_type: u8,
    /// Which elementary stream this describes, matched against the stream
    /// identifier descriptor in the PMT.
    pub component_tag: u8,
    /// Language of the stream, or of its main channel when dual mono.
    pub language: Option<String>,
    /// Language of the second channel, present only on a dual-mono stream
    /// that declares one.
    pub second_language: Option<String>,
}

impl AudioComponent {
    /// Whether this stream carries two languages, one per channel.
    ///
    /// `0x02` is ARIB's "1/0 + 1/0 mode": two independent mono programmes
    /// sharing one stream, which is how Japanese broadcast carries 二か国語.
    #[must_use]
    pub const fn is_dual_mono(&self) -> bool {
        self.component_type == 0x02
    }
}

/// One service, as the recording names itself.
///
/// A recording knows what channel it is: the service id keys everything, and
/// the name is what a person recognises. Making an operator type either of
/// them in is asking them to copy out something the file already says, and to
/// get it right.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ServiceInfo {
    /// Service id, which is the same number the PAT calls the program number.
    pub service_id: u16,
    /// Broadcaster, e.g. 東京.
    pub provider: String,
    /// Channel name, e.g. テレビ朝日.
    pub name: String,
}

/// Service Description Table: what the services in this stream are called.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sdt {
    /// Every service the section described.
    pub services: Vec<ServiceInfo>,
}

impl Sdt {
    /// Parse an SDT section for this transport stream.
    ///
    /// Returns `None` for the table describing *other* streams, which names
    /// services this recording does not contain.
    #[must_use]
    pub fn parse(section: &[u8]) -> Option<Self> {
        if section.first()? != &TABLE_ID_SDT {
            return None;
        }
        let body = section_body(section)?;
        // original_network_id and a reserved byte precede the service loop.
        let mut data = body.get(3..)?;

        let mut services = Vec::new();
        while data.len() >= 5 {
            let service_id = (u16::from(data[0]) << 8) | u16::from(data[1]);
            let length = ((usize::from(data[3]) & 0x0F) << 8) | usize::from(data[4]);
            let Some(descriptors) = data.get(5..5 + length) else {
                break;
            };
            if let Some((provider, name)) = service_name(descriptors) {
                services.push(ServiceInfo {
                    service_id,
                    provider,
                    name,
                });
            }
            data = data.get(5 + length..)?;
        }
        Some(Self { services })
    }
}

/// Find the service descriptor (tag 0x48) and read the two names out of it.
fn service_name(mut descriptors: &[u8]) -> Option<(String, String)> {
    while descriptors.len() >= 2 {
        let tag = descriptors[0];
        let len = usize::from(descriptors[1]);
        let body = descriptors.get(2..2 + len)?;
        if tag == 0x48 {
            // service_type, then two length-prefixed ARIB strings.
            let provider_len = usize::from(*body.get(1)?);
            let provider = body.get(2..2 + provider_len)?;
            let name_len = usize::from(*body.get(2 + provider_len)?);
            let name = body.get(3 + provider_len..3 + provider_len + name_len)?;
            return Some((
                crate::caption::decode_statement(provider),
                crate::caption::decode_statement(name),
            ));
        }
        descriptors = descriptors.get(2 + len..)?;
    }
    None
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

/// Collect every ARIB audio component descriptor (tag 0xC4) in a loop.
///
/// A bilingual programme lists one per audio stream, so this returns all of
/// them rather than the first.
fn find_audio_components(mut descriptors: &[u8]) -> Vec<AudioComponent> {
    let mut found = Vec::new();
    while descriptors.len() >= 2 {
        let tag = descriptors[0];
        let len = usize::from(descriptors[1]);
        let Some(body) = descriptors.get(2..2 + len) else {
            break;
        };
        if tag == 0xC4
            && let Some(component) = parse_audio_component(body)
        {
            found.push(component);
        }
        descriptors = &descriptors[2 + len..];
    }
    found
}

/// Interpret the body of an audio component descriptor.
///
/// ```text
/// 0        4 bits reserved, 4 bits stream_content (0x02 for audio)
/// 1        component_type
/// 2        component_tag
/// 3        stream_type
/// 4        simulcast_group_tag
/// 5        ES_multi_lingual_flag, main_component_flag, quality_indicator,
///          sampling_rate, reserved
/// 6..9     ISO 639 language code
/// 9..12    second ISO 639 language code, only when ES_multi_lingual_flag
/// ```
fn parse_audio_component(body: &[u8]) -> Option<AudioComponent> {
    // Everything up to and including the first language code; the trailing
    // free text is not read.
    if body.len() < 9 {
        return None;
    }
    // Only an audio stream may be described here. A descriptor claiming
    // otherwise is malformed and its component_type would mean something else.
    if body[0] & 0x0F != 0x02 {
        return None;
    }

    let multilingual = body[5] & 0x80 != 0;
    let language = language_code(body.get(6..9));
    let second_language = if multilingual {
        language_code(body.get(9..12))
    } else {
        None
    };

    Some(AudioComponent {
        component_type: body[1],
        component_tag: body[2],
        language,
        second_language,
    })
}

/// Event Information Table: what programme is on, and what it carries.
///
/// Only the present/following table on PID 0x0012 is read, and only for what
/// it says about audio. The schedule tables carry the same descriptors for
/// every programme of the next week, which is of no use to a recording that
/// has already happened.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Eit {
    /// Service the events belong to.
    pub service_id: u16,
    /// Audio components described by the first event in the section, which for
    /// the present/following table is the programme being recorded.
    pub audio: Vec<AudioComponent>,
}

impl Eit {
    /// Parse a present/following EIT section.
    ///
    /// Returns `None` for any other table, including the schedule tables.
    #[must_use]
    pub fn parse(section: &[u8]) -> Option<Self> {
        // 0x4E is this multiplex's own present/following table; 0x4F describes
        // other services and cannot say anything about this recording.
        if section.first()? != &TABLE_ID_EIT_PF {
            return None;
        }
        let service_id = (u16::from(*section.get(3)?) << 8) | u16::from(*section.get(4)?);
        // Section 0 is the present event; section 1 is what follows it, which
        // is a different programme.
        if section.get(6)? != &0 {
            return None;
        }

        let body = section_body(section)?;
        // transport_stream_id, original_network_id, segment_last_section_number
        // and last_table_id, none of which are needed here.
        let events = body.get(6..)?;

        // One event per present/following section in practice, but the loop is
        // written for the general case.
        let mut cursor = 0;
        while cursor + 12 <= events.len() {
            let length =
                ((usize::from(events[cursor + 10]) & 0x0F) << 8) | usize::from(events[cursor + 11]);
            let start = cursor + 12;
            let end = start + length;
            let descriptors = events.get(start..end)?;
            let audio = find_audio_components(descriptors);
            if !audio.is_empty() {
                return Some(Self { service_id, audio });
            }
            cursor = end;
        }

        Some(Self {
            service_id,
            audio: Vec::new(),
        })
    }
}

/// Read a three-byte ISO 639 language code.
///
/// Broadcast fills unused codes with spaces or zeroes rather than omitting
/// them, so anything that is not three letters is treated as absent.
fn language_code(bytes: Option<&[u8]>) -> Option<String> {
    let bytes = bytes?;
    let code: String = bytes
        .iter()
        .map(|&b| char::from(b).to_ascii_lowercase())
        .collect();
    code.chars().all(|c| c.is_ascii_lowercase()).then_some(code)
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

    /// An audio component descriptor, per ARIB STD-B10 part 2, 6.2.26.
    fn audio_descriptor(component_type: u8, tag: u8, languages: &[&str]) -> Vec<u8> {
        let multilingual = languages.len() > 1;
        let mut body = vec![
            0x02, // reserved + stream_content = audio
            component_type,
            tag,
            0x0F, // stream_type
            0x00, // simulcast_group_tag
            if multilingual { 0xB1 } else { 0x31 },
        ];
        for language in languages {
            body.extend_from_slice(language.as_bytes());
        }
        body.push(b'X'); // one byte of the trailing free text
        let mut descriptor = vec![0xC4, u8::try_from(body.len()).expect("short descriptor")];
        descriptor.extend_from_slice(&body);
        descriptor
    }

    /// A present/following EIT section carrying one event and its descriptors.
    fn eit_section(service_id: u16, section_number: u8, descriptors: &[u8]) -> Vec<u8> {
        let length = u16::try_from(descriptors.len()).expect("short descriptor loop");
        let mut event = vec![
            0x00,
            0x01, // event_id
            0x00,
            0x00,
            0x00,
            0x00,
            0x00, // start_time
            0x00,
            0x30,
            0x00, // duration
            0x80 | u8::try_from(length >> 8).expect("fits"),
            u8::try_from(length & 0xFF).expect("fits"),
        ];
        event.extend_from_slice(descriptors);

        let mut body = vec![
            0x00, 0x01, // transport_stream_id
            0x00, 0x02, // original_network_id
            0x00, // segment_last_section_number
            0x4E, // last_table_id
        ];
        body.extend_from_slice(&event);

        let mut section = section(TABLE_ID_EIT_PF, &body);
        // `section` writes a fixed long-form header; the service id and the
        // section number are what this table is keyed on.
        section[3] = u8::try_from(service_id >> 8).expect("fits");
        section[4] = u8::try_from(service_id & 0xFF).expect("fits");
        section[6] = section_number;
        section
    }

    #[test]
    fn reads_a_bilingual_programme_from_the_event_table() {
        // Japanese broadcast carries a bilingual programme as one AAC stream in
        // "1/0 + 1/0 mode", which is indistinguishable from stereo to a
        // decoder. This descriptor is the only place it says otherwise.
        let descriptor = audio_descriptor(0x02, 0x10, &["jpn", "eng"]);
        let eit = Eit::parse(&eit_section(1024, 0, &descriptor)).expect("eit");

        assert_eq!(eit.service_id, 1024);
        assert_eq!(eit.audio.len(), 1);
        assert!(eit.audio[0].is_dual_mono());
        assert_eq!(eit.audio[0].component_tag, 0x10);
        assert_eq!(eit.audio[0].language.as_deref(), Some("jpn"));
        assert_eq!(eit.audio[0].second_language.as_deref(), Some("eng"));
    }

    #[test]
    fn an_ordinary_stereo_programme_is_not_dual_mono() {
        let descriptor = audio_descriptor(0x03, 0x10, &["jpn"]);
        let eit = Eit::parse(&eit_section(1024, 0, &descriptor)).expect("eit");

        assert!(!eit.audio[0].is_dual_mono());
        assert_eq!(eit.audio[0].language.as_deref(), Some("jpn"));
        assert_eq!(eit.audio[0].second_language, None);
    }

    #[test]
    fn the_following_event_is_a_different_programme_and_is_ignored() {
        // Section 1 describes what comes next, which is not what was recorded.
        let descriptor = audio_descriptor(0x02, 0x10, &["jpn", "eng"]);
        assert_eq!(Eit::parse(&eit_section(1024, 1, &descriptor)), None);
    }

    #[test]
    fn a_schedule_table_is_not_read() {
        // 0x50 carries the next week of programmes; none of them is this one.
        let descriptor = audio_descriptor(0x02, 0x10, &["jpn", "eng"]);
        let mut schedule = eit_section(1024, 0, &descriptor);
        schedule[0] = 0x50;
        assert_eq!(Eit::parse(&schedule), None);
    }

    #[test]
    fn a_programme_with_two_audio_streams_lists_both() {
        let mut descriptors = audio_descriptor(0x01, 0x10, &["jpn"]);
        descriptors.extend_from_slice(&audio_descriptor(0x01, 0x11, &["eng"]));
        let eit = Eit::parse(&eit_section(1024, 0, &descriptors)).expect("eit");

        assert_eq!(eit.audio.len(), 2);
        assert_eq!(eit.audio[0].component_tag, 0x10);
        assert_eq!(eit.audio[1].component_tag, 0x11);
    }

    #[test]
    fn a_truncated_or_mislabelled_audio_descriptor_is_ignored() {
        // Too short to hold a language code.
        assert_eq!(parse_audio_component(&[0x02, 0x02, 0x10, 0x0F]), None);
        // stream_content says video, so component_type means something else
        // entirely and reading it as a channel arrangement would be wrong.
        let mut video = audio_descriptor(0x02, 0x10, &["jpn"]);
        video[2] = 0x01;
        assert_eq!(parse_audio_component(&video[2..]), None);
    }

    #[test]
    fn a_blank_language_code_is_treated_as_absent() {
        // Broadcast pads an unused code rather than omitting it.
        assert_eq!(language_code(Some(b"jpn")).as_deref(), Some("jpn"));
        assert_eq!(language_code(Some(b"   ")), None);
        assert_eq!(language_code(Some(&[0, 0, 0])), None);
        assert_eq!(language_code(None), None);
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
