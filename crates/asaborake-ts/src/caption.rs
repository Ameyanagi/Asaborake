//! ARIB B24 closed captions, decoded to text.
//!
//! Japanese broadcast carries subtitles as their own elementary stream, in a
//! character encoding that predates Unicode and is used nowhere else. They are
//! the largest thing Asaborake was throwing away: a recording of a drama keeps
//! its picture and its sound and loses the subtitles entirely.
//!
//! # What this decodes, and what it does not
//!
//! Captions are a full presentation format — positioning, colour, size,
//! flashing, and station-defined glyphs drawn as bitmaps. This reads the text
//! and the line breaks and discards the presentation, because the destination
//! is a subtitle file rather than a reconstruction of the broadcast overlay.
//!
//! The one thing that cannot be recovered is DRCS: characters a station
//! defines as bitmaps for names and logos that are not in any character set.
//! There is nothing to decode them *to*, so they become a placeholder rather
//! than silently vanishing.
//!
//! # Why there is no character table here
//!
//! ARIB's two-byte kanji set is JIS X 0208, in the same cell arrangement that
//! EUC-JP uses with the high bit set. Setting that bit turns a caption byte
//! pair into an EUC-JP one, which `encoding_rs` already knows how to convert.
//! Shipping seven thousand table entries to do the same thing would be seven
//! thousand chances to make a typo.

use crate::packet::TsPacket;
use crate::pes::PtsUnwrapper;

/// One caption, and when it is on screen.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Caption {
    /// When it appears, in seconds from the start of the recording.
    pub start_seconds: f64,
    /// When it goes away.
    pub end_seconds: f64,
    /// The text, with line breaks where the broadcast put them.
    pub text: String,
}

/// How long a caption stays up when nothing says otherwise.
///
/// A caption is cleared either by a control code or by the next one arriving.
/// The last caption of a recording has neither, so it needs a length; five
/// seconds is about how long a line of dialogue is left up.
const DEFAULT_DURATION: f64 = 5.0;

/// The graphic set a byte range currently means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Charset {
    /// ASCII, near enough, plus the yen sign.
    Alphanumeric,
    /// JIS X 0208 rows 4 and 5, one byte per character.
    Hiragana,
    /// As above, the katakana row.
    Katakana,
    /// Two-byte JIS X 0208.
    Kanji,
    /// Station-defined bitmaps, which have no text to decode to.
    Drcs,
}

impl Charset {
    /// Resolve a graphic-set designation byte.
    const fn from_designation(byte: u8) -> Self {
        match byte {
            // Kanji, and the two extension sets that share its arrangement.
            0x42 | 0x39 | 0x3A | 0x3B => Self::Kanji,
            0x30 | 0x31 => Self::Hiragana,
            0x32 => Self::Katakana,
            // Alphanumeric and proportional alphanumeric.
            0x4A | 0x36 => Self::Alphanumeric,
            // Everything else is DRCS, mosaic or macro: bitmaps and layout,
            // with no text behind them.
            _ => Self::Drcs,
        }
    }

    /// Whether characters in this set take two bytes.
    const fn is_two_byte(self) -> bool {
        matches!(self, Self::Kanji)
    }
}

/// The four graphic sets a statement can have designated at once.
///
/// ARIB keeps four slots and shifts between them, rather than switching sets
/// outright, so decoding a caption means tracking all four and which one the
/// printable range currently points at.
#[derive(Debug, Clone, Copy)]
struct Slots {
    sets: [Charset; 4],
    /// Which slot the 0x21-0x7E range means right now.
    left: usize,
}

impl Default for Slots {
    fn default() -> Self {
        // What ARIB specifies at the start of every statement, and what
        // broadcast relies on: kanji, alphanumeric, hiragana, katakana.
        Self {
            sets: [
                Charset::Kanji,
                Charset::Alphanumeric,
                Charset::Hiragana,
                Charset::Katakana,
            ],
            left: 0,
        }
    }
}

impl Slots {
    /// The set printable bytes currently mean.
    const fn current(self) -> Charset {
        self.sets[self.left]
    }

    /// Act on an escape sequence, returning how many bytes it occupied.
    ///
    /// Everything after the escape is structure, not text, so miscounting here
    /// puts a designation byte in the middle of a sentence.
    fn escape(&mut self, rest: &[u8]) -> usize {
        match rest {
            // Two-byte set into a slot: `$` `(`..`+` <set>.
            [0x24, slot @ 0x28..=0x2B, set, ..] => {
                self.sets[usize::from(slot - 0x28)] = Charset::from_designation(*set);
                3
            }
            // Two-byte set into G0, which omits the slot byte.
            [0x24, set, ..] if !(0x28..=0x2B).contains(set) => {
                self.sets[0] = Charset::from_designation(*set);
                2
            }
            // Single-byte set into a slot.
            [slot @ 0x28..=0x2B, set, ..] => {
                self.sets[usize::from(slot - 0x28)] = Charset::from_designation(*set);
                2
            }
            // Locking shift into G2 or G3.
            [0x6E, ..] => {
                self.left = 2;
                1
            }
            [0x6F, ..] => {
                self.left = 3;
                1
            }
            _ => 1,
        }
    }
}

/// What an unmapped station-defined glyph becomes.
///
/// Visible rather than silent: a caption reading "〓さん" tells a reader a
/// character was there and could not be rendered, where dropping it would
/// quietly change what was said.
const DRCS_PLACEHOLDER: char = '〓';

/// Decode one caption statement body into text.
///
/// The body is a stream of characters interleaved with control codes. Only the
/// codes that affect *what* is written are acted on — line breaks, spaces, and
/// the parameterised ones whose arguments have to be skipped so their bytes
/// are not mistaken for text.
#[must_use]
pub fn decode_statement(body: &[u8]) -> String {
    let mut out = String::new();
    let mut slots = Slots::default();
    // Set by a single shift, and spent on the very next character.
    let mut single_shift: Option<usize> = None;
    let mut index = 0;

    while index < body.len() {
        let byte = body[index];
        match byte {
            // C0 control codes.
            0x00..=0x20 => {
                index += 1;
                match byte {
                    // Space.
                    0x20 => out.push(' '),
                    // Active position return starts a new line; clear screen
                    // separates one caption from the next. Both mean the text
                    // so far is finished.
                    0x0C | 0x0D => push_line_break(&mut out),
                    // Locking shifts: point the printable range at another
                    // slot. Broadcast uses these to move between kanji and
                    // alphanumeric within a line.
                    0x0F => slots.left = 0,
                    0x0E => slots.left = 1,
                    0x1B => {
                        index += slots.escape(body.get(index..).unwrap_or_default());
                    }
                    // Active position set takes two parameter bytes.
                    0x1C => index += 2,
                    // Active position forward takes one parameter byte.
                    0x16 => index += 1,
                    // Single shifts: the *next character only* comes from G2
                    // or G3. They carry no parameter, and treating them as if
                    // they did eats the character they were pointing at.
                    0x19 => single_shift = Some(2),
                    0x1D => single_shift = Some(3),
                    _ => {}
                }
            }
            // C1 control codes, most of which take parameters that must not be
            // read as text.
            0x80..=0xA0 => {
                index += 1;
                match byte {
                    // Colour takes one parameter — unless that parameter is
                    // 0x20, which introduces a colour-map entry and is itself
                    // followed by the entry. Broadcast opens most captions
                    // with exactly this, and skipping one byte instead of two
                    // put a stray kanji at the front of every single one.
                    0x90 => {
                        index += if body.get(index) == Some(&0x20) { 2 } else { 1 };
                    }
                    // One parameter byte: the second size control, flashing,
                    // concealment, pattern polarity, writing mode, macro,
                    // highlighting, repeat.
                    0x8B | 0x91..=0x95 | 0x97 | 0x98 => index += 1,
                    // Time control: a mode byte and a value.
                    0x9D => index += 2,
                    // A control sequence: parameters, then a final byte that
                    // says which control it was.
                    0x9B => index += csi_length(body.get(index..).unwrap_or_default()),
                    // Everything else — the eight foreground colours above
                    // all — carries its argument in the code itself, so there
                    // is nothing after it to skip. Skipping a byte here would
                    // swallow the first character of the line.
                    _ => {}
                }
            }
            // Graphic characters.
            _ => {
                let charset = single_shift
                    .take()
                    .map_or_else(|| slots.current(), |slot| slots.sets[slot]);
                let bytes_used = if charset.is_two_byte() { 2 } else { 1 };
                if index + bytes_used > body.len() {
                    break;
                }
                push_character(&mut out, charset, &body[index..index + bytes_used]);
                index += bytes_used;
            }
        }
    }

    out.trim_end().to_owned()
}

/// Append a line break, without opening the caption with one.
fn push_line_break(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// Convert one character and append it.
fn push_character(out: &mut String, charset: Charset, bytes: &[u8]) {
    match charset {
        Charset::Drcs => out.push(DRCS_PLACEHOLDER),
        Charset::Alphanumeric => {
            let byte = bytes[0] & 0x7F;
            if byte.is_ascii_graphic() {
                out.push(char::from(byte));
            }
        }
        // The single-byte kana sets are rows 4 and 5 of the same standard, so
        // they decode through the same path with the row supplied.
        Charset::Hiragana | Charset::Katakana => {
            let row = if charset == Charset::Hiragana {
                0x24
            } else {
                0x25
            };
            push_jis(out, row, bytes[0] & 0x7F);
        }
        Charset::Kanji => push_jis(out, bytes[0] & 0x7F, bytes[1] & 0x7F),
    }
}

/// Convert a JIS X 0208 cell to a character, via EUC-JP.
///
/// Setting the high bit of both bytes is exactly what EUC-JP is, so this hands
/// the pair to a decoder that already exists rather than carrying a table.
fn push_jis(out: &mut String, first: u8, second: u8) {
    if !(0x21..=0x7E).contains(&first) || !(0x21..=0x7E).contains(&second) {
        return;
    }
    let euc = [first | 0x80, second | 0x80];
    let (text, _, had_errors) = encoding_rs::EUC_JP.decode(&euc);
    if had_errors {
        // A cell EUC-JP does not define is an ARIB extension — mostly the
        // station and programme symbols in rows 85 onwards.
        out.push(DRCS_PLACEHOLDER);
    } else {
        out.push_str(&text);
    }
}

/// How many bytes a control sequence occupies after its introducer.
///
/// Parameters are digits and separators; the sequence ends at the final byte
/// that says which control it was. Stopping early would leave that final byte
/// to be read as a character.
fn csi_length(rest: &[u8]) -> usize {
    rest.iter()
        .position(|b| (0x40..=0x6F).contains(b))
        .map_or(rest.len(), |at| at + 1)
}

/// Pull the caption statement bodies out of one PES payload.
///
/// The payload is a data group holding data units; only the units carrying a
/// statement body have text in them.
#[must_use]
pub fn statements_in(payload: &[u8]) -> Vec<Vec<u8>> {
    let mut found = Vec::new();

    // Synchronised (0x80) and asynchronous (0x81) PES data. Anything else on
    // this PID is not a caption.
    let Some(&identifier) = payload.first() else {
        return found;
    };
    if identifier != 0x80 && identifier != 0x81 {
        return found;
    }
    // data_identifier, private_stream_id, then a length whose low 8 bits are
    // the header length, then that many bytes of header.
    let Some(&header_length) = payload.get(2) else {
        return found;
    };
    let group_start = 3 + usize::from(header_length & 0x0F);

    let Some(group) = payload.get(group_start..) else {
        return found;
    };
    // data_group_id and version, then a link number, a sequence number, and a
    // 16-bit length.
    if group.len() < 5 {
        return found;
    }
    let group_size = (usize::from(group[3]) << 8) | usize::from(group[4]);
    let Some(mut data) = group.get(5..5 + group_size) else {
        return found;
    };

    // Caption management data begins with the language table; caption
    // statement data begins straight at the units. Both end with a units
    // length, so the units are found by walking from the end of the header.
    let group_id = group[0] >> 2;
    // 0x00 and 0x20 are the management groups, which carry no statements.
    if group_id == 0x00 || group_id == 0x20 {
        return found;
    }
    // TMD occupies the first byte; when it signals a time, three more bytes
    // follow. Then a 24-bit data unit loop length.
    let time_mode = data.first().map_or(0, |b| b >> 6);
    let header = if time_mode == 1 || time_mode == 2 {
        5
    } else {
        1
    };
    let Some(rest) = data.get(header..) else {
        return found;
    };
    if rest.len() < 3 {
        return found;
    }
    let units_length =
        (usize::from(rest[0]) << 16) | (usize::from(rest[1]) << 8) | usize::from(rest[2]);
    let Some(units) = rest.get(3..3 + units_length.min(rest.len() - 3)) else {
        return found;
    };
    data = units;

    // Each unit: a separator, a parameter saying what it is, a 24-bit length,
    // then the body.
    let mut index = 0;
    while index + 5 <= data.len() {
        let parameter = data[index + 1];
        let length = (usize::from(data[index + 2]) << 16)
            | (usize::from(data[index + 3]) << 8)
            | usize::from(data[index + 4]);
        let body_start = index + 5;
        let Some(body) = data.get(body_start..body_start + length) else {
            break;
        };
        // 0x20 is the statement body; the others are bitmaps and geometry.
        if parameter == 0x20 {
            found.push(body.to_vec());
        }
        index = body_start + length;
    }

    found
}

/// Turn decoded statements and their timestamps into captions with durations.
///
/// A caption ends when the next one begins, because that is what the broadcast
/// does: there is no "off" for a line that is simply replaced.
#[must_use]
pub fn assemble(mut entries: Vec<(f64, String)>) -> Vec<Caption> {
    entries.retain(|(_, text)| !text.trim().is_empty());
    entries.sort_by(|a, b| a.0.total_cmp(&b.0));

    let mut captions: Vec<Caption> = Vec::with_capacity(entries.len());
    for (index, (start, text)) in entries.iter().enumerate() {
        let end = entries
            .get(index + 1)
            .map_or(start + DEFAULT_DURATION, |(next, _)| *next);
        // A statement repeated at the same instant is the same caption sent
        // twice, which broadcast does for reliability.
        if captions
            .last()
            .is_some_and(|last| last.text == *text && (start - last.start_seconds).abs() < 0.1)
        {
            continue;
        }
        captions.push(Caption {
            start_seconds: *start,
            end_seconds: end.max(start + 0.1),
            text: text.clone(),
        });
    }
    captions
}

/// Render captions as a `SubRip` file.
///
/// SRT because everything plays it — every browser, every television, every
/// player — and a subtitle nobody can turn on is no better than none.
#[must_use]
pub fn to_srt(captions: &[Caption]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for (index, caption) in captions.iter().enumerate() {
        // Writing to a String cannot fail; the result is discarded because
        // there is nothing it could mean.
        let _ = writeln!(out, "{}", index + 1);
        let _ = writeln!(
            out,
            "{} --> {}",
            srt_time(caption.start_seconds),
            srt_time(caption.end_seconds)
        );
        out.push_str(&caption.text);
        out.push_str("\n\n");
    }
    out
}

/// A timestamp in `SubRip`'s `hh:mm:ss,mmm`.
fn srt_time(seconds: f64) -> String {
    let seconds = seconds.max(0.0);
    let whole = seconds.trunc() as u64;
    let millis = ((seconds - seconds.trunc()) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        whole / 3600,
        (whole % 3600) / 60,
        whole % 60,
        millis.min(999)
    )
}

/// Read every caption out of a transport stream.
///
/// A second pass over the file rather than a hitch-hiker on the inventory
/// scan: captions are wanted only when they are asked for, and a recording
/// without them should not pay for reassembling PES packets that are not
/// there.
///
/// # Errors
/// Returns [`Error::NoSync`](crate::Error::NoSync) when the input is not a
/// transport stream, or [`Error::Io`](crate::Error::Io) when reading fails.
pub fn extract<R: std::io::Read>(mut reader: R) -> Result<Vec<Caption>, crate::Error> {
    use crate::packet::detect_layout;

    const CHUNK: usize = 188 * 1024;

    let mut buffer = Vec::with_capacity(CHUNK * 2);
    let mut scratch = vec![0u8; CHUNK];

    let layout = loop {
        let read = reader.read(&mut scratch).map_err(crate::Error::Io)?;
        if read == 0 {
            return Err(crate::Error::NoSync);
        }
        buffer.extend_from_slice(&scratch[..read]);
        match detect_layout(&buffer) {
            Ok((layout, start)) => {
                buffer.drain(..start);
                break layout;
            }
            Err(_) if buffer.len() < CHUNK * 4 => {}
            Err(error) => return Err(error),
        }
    };

    let mut state = Extractor::default();
    let stride = layout.stride();
    let sync = layout.sync_offset();

    loop {
        let mut consumed = 0usize;
        while consumed + stride <= buffer.len() {
            let raw = &buffer[consumed + sync..consumed + sync + 188];
            if let Some(packet) = TsPacket::parse(raw) {
                state.push(&packet);
                consumed += stride;
            } else {
                consumed += 1;
                if let Some(offset) = crate::scan::resync(&buffer[consumed..], layout) {
                    consumed += offset;
                } else {
                    break;
                }
            }
        }
        buffer.drain(..consumed);

        let read = reader.read(&mut scratch).map_err(crate::Error::Io)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&scratch[..read]);
    }

    Ok(state.finish())
}

/// Accumulates caption PES packets while a transport stream is read.
#[derive(Default)]
struct Extractor {
    /// PIDs the PMT said carry captions.
    caption_pids: std::collections::BTreeSet<u16>,
    pat_assembler: crate::psi::SectionAssembler,
    pmt_assemblers: std::collections::HashMap<u16, crate::psi::SectionAssembler>,
    pmt_pids: std::collections::BTreeSet<u16>,

    /// The caption PES packet being reassembled, and its timestamp.
    unit: Vec<u8>,
    unit_pts: Option<i64>,

    pts: PtsUnwrapper,
    /// First timestamp seen on any stream, which is time zero.
    first_pts: Option<i64>,
    found: Vec<(f64, String)>,
    /// Statements whose timestamp could not be resolved until later.
    pending: Vec<(i64, String)>,
}

impl Extractor {
    fn push(&mut self, packet: &TsPacket<'_>) {
        if packet.pid == crate::packet::PID_PAT {
            for section in self.pat_assembler.push(packet) {
                if let Some(pat) = crate::psi::Pat::parse(&section) {
                    for (_, pmt_pid) in pat.programs {
                        self.pmt_pids.insert(pmt_pid);
                        self.pmt_assemblers.entry(pmt_pid).or_default();
                    }
                }
            }
            return;
        }

        if self.pmt_pids.contains(&packet.pid) {
            let sections = self
                .pmt_assemblers
                .entry(packet.pid)
                .or_default()
                .push(packet);
            for section in sections {
                if let Some(pmt) = crate::psi::Pmt::parse(&section) {
                    for es in &pmt.streams {
                        if crate::psi::StreamKind::resolve(es.stream_type, es.component_tag)
                            == crate::psi::StreamKind::Caption
                        {
                            self.caption_pids.insert(es.pid);
                        }
                    }
                }
            }
            return;
        }

        // Time zero is the first timestamp in the recording, whichever stream
        // it arrives on, so captions line up with the video rather than with
        // whenever the caption stream happened to start.
        if let Some(header) = packet
            .payload_unit_start
            .then(|| crate::pes::PesHeader::parse(packet.payload))
            .flatten()
            && let Some(raw) = header.pts
        {
            let unwrapped = self.pts.push(raw);
            self.first_pts.get_or_insert(unwrapped);
        }

        if !self.caption_pids.contains(&packet.pid) || !packet.has_payload() {
            return;
        }

        if packet.payload_unit_start {
            self.flush();
            if let Some(header) = crate::pes::PesHeader::parse(packet.payload) {
                self.unit_pts = header.pts.map(|raw| self.pts.push(raw));
                self.unit
                    .extend_from_slice(&packet.payload[header.payload_offset..]);
            }
        } else if !self.unit.is_empty() {
            self.unit.extend_from_slice(packet.payload);
        }
    }

    /// Decode the caption packet just completed.
    fn flush(&mut self) {
        let unit = std::mem::take(&mut self.unit);
        let Some(pts) = self.unit_pts.take() else {
            return;
        };
        if unit.is_empty() {
            return;
        }
        for statement in statements_in(&unit) {
            let text = decode_statement(&statement);
            if !text.trim().is_empty() {
                self.pending.push((pts, text));
            }
        }
    }

    fn finish(mut self) -> Vec<Caption> {
        self.flush();
        let zero = self.first_pts.unwrap_or(0);
        self.found.extend(
            self.pending
                .into_iter()
                .map(|(pts, text)| (PtsUnwrapper::to_seconds(pts - zero), text)),
        );
        assemble(self.found)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::float_cmp)]

    use super::*;

    /// Encode a string of JIS X 0208 kanji cells as a statement body.
    fn kanji(cells: &[(u8, u8)]) -> Vec<u8> {
        cells.iter().flat_map(|(a, b)| [*a, *b]).collect()
    }

    #[test]
    fn decodes_kanji_through_the_euc_jp_arrangement() {
        // Row 0x24 is hiragana in JIS X 0208; cell 0x22 is あ.
        let body = kanji(&[(0x24, 0x22), (0x24, 0x24), (0x24, 0x26)]);
        assert_eq!(decode_statement(&body), "あいう");
    }

    #[test]
    fn a_carriage_return_becomes_a_line_break() {
        let mut body = kanji(&[(0x24, 0x22)]);
        body.push(0x0D);
        body.extend_from_slice(&kanji(&[(0x24, 0x24)]));
        assert_eq!(decode_statement(&body), "あ\nい");
    }

    #[test]
    fn a_caption_does_not_open_with_a_blank_line() {
        // Broadcast routinely sends a clear or a return before the first
        // character; a subtitle file starting with an empty line looks broken.
        let mut body = vec![0x0C, 0x0D];
        body.extend_from_slice(&kanji(&[(0x24, 0x22)]));
        assert_eq!(decode_statement(&body), "あ");
    }

    #[test]
    fn a_station_defined_glyph_is_marked_rather_than_dropped() {
        // Dropping it would quietly change what the caption said.
        let body = vec![0x1B, 0x24, 0x28, 0x40, 0x21];
        assert_eq!(decode_statement(&body), DRCS_PLACEHOLDER.to_string());
    }

    #[test]
    fn a_locking_shift_moves_between_kanji_and_letters() {
        // Broadcast switches sets mid-line constantly; a caption reading
        // "NHKニュース" is two sets in one statement.
        let mut body = vec![0x0E]; // shift to G1, which is alphanumeric
        body.extend_from_slice(b"NHK");
        body.push(0x0F); // back to G0, which is kanji
        body.extend_from_slice(&kanji(&[(0x24, 0x22)]));
        assert_eq!(decode_statement(&body), "NHKあ");
    }

    #[test]
    fn a_colour_map_entry_takes_both_of_its_bytes() {
        // Taken from a real recording: broadcast opens almost every caption
        // with COL 0x20 <entry>. Skipping one byte instead of two left a
        // stray kanji at the front of every caption in the file.
        let mut body = vec![0x90, 0x20, 0x44];
        body.extend_from_slice(&kanji(&[(0x24, 0x22)]));
        assert_eq!(decode_statement(&body), "あ");

        // The ordinary form still takes exactly one.
        let mut plain = vec![0x90, 0x51];
        plain.extend_from_slice(&kanji(&[(0x24, 0x22)]));
        assert_eq!(decode_statement(&plain), "あ");
    }

    #[test]
    fn a_single_shift_redirects_one_character_and_no_more() {
        // SS2 points at G2, which starts as hiragana. The character after
        // that comes from G0 again.
        let mut body = vec![0x19, 0x22];
        body.extend_from_slice(&kanji(&[(0x24, 0x24)]));
        assert_eq!(decode_statement(&body), "あい");
    }

    #[test]
    fn a_designation_replaces_the_set_in_its_slot() {
        // Designate hiragana into G0, then print a single-byte cell from it.
        let body = vec![0x1B, 0x28, 0x30, 0x22];
        assert_eq!(decode_statement(&body), "あ");
    }

    #[test]
    fn a_parameterised_control_does_not_eat_the_next_character() {
        // COL takes one parameter byte. Skipping two would swallow the first
        // half of the kanji cell after it and render a different character.
        let mut body = vec![0x90, 0x40];
        body.extend_from_slice(&kanji(&[(0x24, 0x22)]));
        assert_eq!(decode_statement(&body), "あ");
    }

    #[test]
    fn a_colour_control_carries_its_argument_in_itself() {
        // The eight foreground colours have nothing after them. Treating them
        // as parameterised swallows the first character of the caption.
        let mut body = vec![0x83];
        body.extend_from_slice(&kanji(&[(0x24, 0x22)]));
        assert_eq!(decode_statement(&body), "あ");
    }

    #[test]
    fn a_control_sequence_is_skipped_up_to_its_final_byte() {
        // CSI: parameters, an intermediate, then the byte naming the control.
        let mut body = vec![0x9B, 0x33, 0x30, 0x3B, 0x33, 0x30, 0x20, 0x53];
        body.extend_from_slice(&kanji(&[(0x24, 0x22)]));
        assert_eq!(decode_statement(&body), "あ");
    }

    #[test]
    fn an_escape_sequence_is_skipped_whole() {
        // Two-byte set into G0: ESC $ ( <set>. Four bytes, none of them text.
        let mut body = vec![0x1B, 0x24, 0x28, 0x39];
        body.extend_from_slice(&kanji(&[(0x24, 0x22)]));
        let text = decode_statement(&body);
        assert!(!text.contains('('), "the designation leaked: {text:?}");
    }

    #[test]
    fn a_truncated_character_does_not_run_off_the_end() {
        // A statement cut short by a lost packet must not panic.
        assert_eq!(decode_statement(&[0x24]), "");
        assert_eq!(decode_statement(&[]), "");
    }

    #[test]
    fn each_caption_lasts_until_the_next_one() {
        let captions = assemble(vec![
            (1.0, "one".to_owned()),
            (4.0, "two".to_owned()),
            (9.0, "three".to_owned()),
        ]);

        assert_eq!(captions.len(), 3);
        assert_eq!(captions[0].end_seconds, 4.0);
        assert_eq!(captions[1].end_seconds, 9.0);
        // The last has nothing after it to be ended by.
        assert_eq!(captions[2].end_seconds, 9.0 + DEFAULT_DURATION);
    }

    #[test]
    fn a_caption_repeated_for_reliability_appears_once() {
        let captions = assemble(vec![
            (1.0, "same".to_owned()),
            (1.02, "same".to_owned()),
            (5.0, "different".to_owned()),
        ]);
        assert_eq!(captions.len(), 2, "{captions:?}");
    }

    #[test]
    fn empty_statements_are_not_captions() {
        let captions = assemble(vec![(1.0, String::new()), (2.0, "   ".to_owned())]);
        assert!(captions.is_empty());
    }

    #[test]
    fn renders_subrip_with_the_timestamps_players_expect() {
        let srt = to_srt(&[Caption {
            start_seconds: 3661.5,
            end_seconds: 3663.25,
            text: "こんにちは".to_owned(),
        }]);

        assert_eq!(srt, "1\n01:01:01,500 --> 01:01:03,250\nこんにちは\n\n");
    }

    #[test]
    fn a_payload_that_is_not_a_caption_yields_nothing() {
        assert!(statements_in(&[]).is_empty());
        assert!(statements_in(&[0x00, 0xFF, 0x00]).is_empty());
    }

    #[test]
    fn a_statement_unit_is_found_inside_a_data_group() {
        // data_identifier, private_stream_id, header length 0.
        let mut payload = vec![0x80, 0xFF, 0x00];
        // Statement body: two kanji cells.
        let body = kanji(&[(0x24, 0x22), (0x24, 0x24)]);
        // One data unit: separator, parameter 0x20, 24-bit length, body.
        let mut units = vec![0x1F, 0x20];
        units.extend_from_slice(&[0, 0, u8::try_from(body.len()).expect("short")]);
        units.extend_from_slice(&body);
        // Group body: TMD byte, 24-bit units length, units.
        let mut group_body = vec![0x00];
        group_body.extend_from_slice(&[0, 0, u8::try_from(units.len()).expect("short")]);
        group_body.extend_from_slice(&units);
        // Group header: id (statement, not management), version, link, length.
        let mut group = vec![0x04, 0x00];
        group.extend_from_slice(&[0, u8::try_from(group_body.len()).expect("short")]);
        group.insert(2, 0x00);
        group.truncate(5);
        group[3] = 0;
        group[4] = u8::try_from(group_body.len()).expect("short");
        group.extend_from_slice(&group_body);
        payload.extend_from_slice(&group);

        let statements = statements_in(&payload);
        assert_eq!(statements.len(), 1, "{statements:?}");
        assert_eq!(decode_statement(&statements[0]), "あい");
    }
}
