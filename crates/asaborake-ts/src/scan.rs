//! A single streaming pass over a recording that produces its inventory.
//!
//! Everything downstream — which PIDs to hand ffmpeg, how long the recording
//! really is, whether it needs splitting, whether the CAS worked — comes from
//! this pass. It reads the file once and holds no frame data, so it stays
//! cheap even on a three-hour recording.

use std::collections::{BTreeMap, HashMap};
use std::io::Read;

use crate::Error;
use crate::packet::{ContinuityTracker, PID_NULL, PID_PAT, PacketLayout, TsPacket, detect_layout};
use crate::pes::{PesHeader, PtsUnwrapper};
use crate::psi::{Pat, Pmt, SectionAssembler, StreamKind};
use crate::video::{VideoFormat, parse_h264_sps, parse_mpeg2_sequence_header};

/// Counters describing how healthy the recording is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TsStats {
    /// Packets the demodulator flagged as uncorrectable.
    pub error_packets: u64,
    /// Packets inferred lost from continuity-counter gaps.
    pub dropped_packets: u64,
    /// Packets still scrambled, meaning decryption did not happen.
    pub scrambled_packets: u64,
    /// Signalled timebase discontinuities.
    pub discontinuities: u64,
    /// Stuffing packets, excluded from every other count.
    pub null_packets: u64,
    /// Packets that failed to parse and forced a resync.
    pub corrupt_packets: u64,
}

impl TsStats {
    /// Whether the recording looks unusable rather than merely imperfect.
    ///
    /// A handful of drops is normal on terrestrial reception; a stream that is
    /// substantially scrambled means the CAS never worked and no amount of
    /// analysis downstream will recover it.
    #[must_use]
    pub fn is_severely_damaged(&self, total_packets: u64) -> bool {
        if total_packets == 0 {
            return true;
        }
        let scrambled_ratio = self.scrambled_packets as f64 / total_packets as f64;
        scrambled_ratio > 0.05
    }
}

/// One elementary stream as advertised by the PMT.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StreamInfo {
    /// PID carrying this stream.
    pub pid: u16,
    /// Raw stream type from the PMT.
    pub stream_type: u8,
    /// Resolved meaning of that stream type.
    pub kind: StreamKind,
    /// ARIB component tag, when the PMT carried one.
    pub component_tag: Option<u8>,
}

/// One program carried in the transport stream.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProgramInfo {
    /// Program number from the PAT, i.e. the service id.
    pub program_number: u16,
    /// PID the PMT for this program is carried on.
    pub pmt_pid: u16,
    /// PID carrying the program clock reference.
    pub pcr_pid: u16,
    /// Elementary streams belonging to this program.
    pub streams: Vec<StreamInfo>,
}

impl ProgramInfo {
    /// The primary video stream, if this program has one.
    #[must_use]
    pub fn video(&self) -> Option<&StreamInfo> {
        self.streams.iter().find(|s| s.kind.is_video())
    }

    /// Every audio stream, in PMT order. Dual-mono programmes list two.
    #[must_use]
    pub fn audio(&self) -> Vec<&StreamInfo> {
        self.streams.iter().filter(|s| s.kind.is_audio()).collect()
    }
}

/// A point where the video format changed mid-recording.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FormatChange {
    /// Position in the recording, in seconds from the first video timestamp.
    pub seconds: f64,
    /// The format in effect from this point.
    pub format: VideoFormat,
}

/// The complete inventory of a recording.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TsInfo {
    /// Packet layout the file uses on disk.
    pub layout: PacketLayout,
    /// Total packets read.
    pub packet_count: u64,
    /// Size of the file in bytes.
    pub file_size: u64,
    /// Duration derived from the span of video presentation timestamps.
    pub duration_seconds: f64,
    /// Programs carried, in PAT order.
    pub programs: Vec<ProgramInfo>,
    /// Video format at the start of the recording, if a video stream was found.
    pub video_format: Option<VideoFormat>,
    /// Every format change after the first, in time order.
    pub format_changes: Vec<FormatChange>,
    /// Health counters.
    pub stats: TsStats,
}

impl TsInfo {
    /// The program Asaborake should operate on.
    ///
    /// A recording made by `EPGStation` contains a single service, but a raw
    /// full-transponder capture can carry several. Preferring the program with
    /// a video stream, then the lowest program number, picks the main service
    /// rather than a data or radio sub-channel.
    #[must_use]
    pub fn primary_program(&self) -> Option<&ProgramInfo> {
        self.programs
            .iter()
            .filter(|p| p.video().is_some())
            .min_by_key(|p| p.program_number)
            .or_else(|| self.programs.first())
    }

    /// Whether the recording changes geometry and therefore needs splitting.
    #[must_use]
    pub fn requires_split(&self) -> bool {
        let Some(initial) = self.video_format else {
            return false;
        };
        self.format_changes
            .iter()
            .any(|change| initial.requires_split(&change.format))
    }
}

/// Read a transport stream and produce its inventory.
///
/// # Errors
/// Returns [`Error::NoSync`] when the input is not a transport stream, or
/// [`Error::Io`] when reading fails.
pub fn scan<R: Read>(mut reader: R, file_size: u64) -> Result<TsInfo, Error> {
    // Large enough to hold many packets per read at any layout, so the sync
    // search and the per-packet loop both work on whole packets.
    const CHUNK: usize = 188 * 1024;

    let mut buffer = Vec::with_capacity(CHUNK * 2);
    let mut scratch = vec![0u8; CHUNK];

    // Read enough to identify the layout before entering the main loop.
    let layout = loop {
        let read = reader.read(&mut scratch).map_err(Error::Io)?;
        if read == 0 {
            return Err(Error::NoSync);
        }
        buffer.extend_from_slice(&scratch[..read]);
        match detect_layout(&buffer) {
            Ok((layout, start)) => {
                buffer.drain(..start);
                break layout;
            }
            // Keep reading; a long run of leading noise is unusual but legal.
            Err(_) if buffer.len() < CHUNK * 4 => {}
            Err(error) => return Err(error),
        }
    };

    let mut state = ScanState::new(layout);
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
                // Lost alignment. Step one byte and let the next iteration
                // re-find the sync byte rather than discarding the rest.
                state.stats.corrupt_packets += 1;
                consumed += 1;
                if let Some(offset) = resync(&buffer[consumed..], layout) {
                    consumed += offset;
                } else {
                    break;
                }
            }
        }
        buffer.drain(..consumed);

        let read = reader.read(&mut scratch).map_err(Error::Io)?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&scratch[..read]);
    }

    Ok(state.finish(file_size))
}

/// Find the next offset where a packet plausibly starts.
fn resync(buf: &[u8], layout: PacketLayout) -> Option<usize> {
    let stride = layout.stride();
    let sync = layout.sync_offset();
    (0..buf.len().saturating_sub(stride)).find(|&offset| {
        buf.get(offset + sync) == Some(&crate::packet::SYNC_BYTE)
            && buf.get(offset + stride + sync) == Some(&crate::packet::SYNC_BYTE)
    })
}

/// Mutable state carried across the scanning pass.
struct ScanState {
    layout: PacketLayout,
    packet_count: u64,
    stats: TsStats,
    continuity: ContinuityTracker,

    pat_assembler: SectionAssembler,
    pmt_assemblers: HashMap<u16, SectionAssembler>,
    /// `program_number` -> `pmt_pid`, from the PAT.
    pat: BTreeMap<u16, u16>,
    /// `pmt_pid` -> parsed PMT.
    pmts: BTreeMap<u16, Pmt>,

    video_pid: Option<u16>,
    video_kind: Option<StreamKind>,
    /// Partial elementary-stream bytes for the current video access unit.
    video_unit: Vec<u8>,
    video_unit_pts: Option<i64>,

    pts: PtsUnwrapper,
    first_pts: Option<i64>,
    last_pts: Option<i64>,

    /// Format at the head of the recording, latched once and never revised.
    initial_format: Option<VideoFormat>,
    current_format: Option<VideoFormat>,
    format_changes: Vec<FormatChange>,
}

impl ScanState {
    fn new(layout: PacketLayout) -> Self {
        Self {
            layout,
            packet_count: 0,
            stats: TsStats::default(),
            continuity: ContinuityTracker::new(),
            pat_assembler: SectionAssembler::new(),
            pmt_assemblers: HashMap::new(),
            pat: BTreeMap::new(),
            pmts: BTreeMap::new(),
            video_pid: None,
            video_kind: None,
            video_unit: Vec::new(),
            video_unit_pts: None,
            pts: PtsUnwrapper::new(),
            first_pts: None,
            last_pts: None,
            initial_format: None,
            current_format: None,
            format_changes: Vec::new(),
        }
    }

    fn push(&mut self, packet: &TsPacket<'_>) {
        self.packet_count += 1;

        if packet.pid == PID_NULL {
            self.stats.null_packets += 1;
            return;
        }
        if packet.transport_error {
            self.stats.error_packets += 1;
            return;
        }
        if packet.discontinuity {
            self.stats.discontinuities += 1;
        }
        if packet.is_scrambled() {
            self.stats.scrambled_packets += 1;
        }
        self.stats.dropped_packets += u64::from(self.continuity.push(packet));

        match packet.pid {
            PID_PAT => self.handle_pat(packet),
            pid if self.pat.values().any(|&p| p == pid) => self.handle_pmt(pid, packet),
            pid if Some(pid) == self.video_pid => self.handle_video(packet),
            _ => {}
        }
    }

    fn handle_pat(&mut self, packet: &TsPacket<'_>) {
        for section in self.pat_assembler.push(packet) {
            let Some(pat) = Pat::parse(&section) else {
                continue;
            };
            for (program_number, pmt_pid) in pat.programs {
                self.pat.insert(program_number, pmt_pid);
                self.pmt_assemblers.entry(pmt_pid).or_default();
            }
        }
    }

    fn handle_pmt(&mut self, pid: u16, packet: &TsPacket<'_>) {
        let sections = self.pmt_assemblers.entry(pid).or_default().push(packet);
        for section in sections {
            let Some(pmt) = Pmt::parse(&section) else {
                continue;
            };
            // Latch the first video PID we see and stay with it; broadcast
            // repeats the PMT constantly and re-latching would reset the
            // access-unit accumulator on every repeat.
            if self.video_pid.is_none()
                && let Some(es) = pmt
                    .streams
                    .iter()
                    .find(|es| StreamKind::resolve(es.stream_type, es.component_tag).is_video())
            {
                self.video_pid = Some(es.pid);
                self.video_kind = Some(StreamKind::resolve(es.stream_type, es.component_tag));
            }
            self.pmts.insert(pid, pmt);
        }
    }

    fn handle_video(&mut self, packet: &TsPacket<'_>) {
        if packet.is_scrambled() || !packet.has_payload() {
            return;
        }

        if packet.payload_unit_start {
            // The previous access unit is complete; inspect it before starting
            // the next one.
            self.flush_video_unit();
            if let Some(header) = PesHeader::parse(packet.payload) {
                if let Some(raw) = header.pts {
                    let unwrapped = self.pts.push(raw);
                    self.first_pts.get_or_insert(unwrapped);
                    self.last_pts = Some(unwrapped);
                    self.video_unit_pts = Some(unwrapped);
                }
                if let Some(rest) = packet.payload.get(header.payload_offset..) {
                    self.video_unit.extend_from_slice(rest);
                }
            }
        } else if !self.video_unit.is_empty() {
            // Only the head of an access unit carries the sequence header, so
            // there is nothing to gain from buffering a whole GOP.
            const HEAD: usize = 2048;
            if self.video_unit.len() < HEAD {
                self.video_unit.extend_from_slice(packet.payload);
            }
        }
    }

    fn flush_video_unit(&mut self) {
        if self.video_unit.is_empty() {
            return;
        }
        let unit = std::mem::take(&mut self.video_unit);
        let parsed = match self.video_kind {
            Some(StreamKind::H264Video) => parse_h264_sps(&unit),
            // HEVC geometry parsing is not implemented; the format-change
            // check simply does not fire for 4K services.
            Some(StreamKind::HevcVideo) => None,
            _ => parse_mpeg2_sequence_header(&unit),
        };
        let Some(format) = parsed else {
            return;
        };

        match self.current_format {
            None => {
                self.initial_format = Some(format);
                self.current_format = Some(format);
            }
            Some(current) if current != format => {
                let seconds = self
                    .video_unit_pts
                    .zip(self.first_pts)
                    .map_or(0.0, |(now, first)| PtsUnwrapper::to_seconds(now - first));
                self.format_changes.push(FormatChange { seconds, format });
                self.current_format = Some(format);
            }
            Some(_) => {}
        }
    }

    fn finish(mut self, file_size: u64) -> TsInfo {
        self.flush_video_unit();

        let duration_seconds = self
            .first_pts
            .zip(self.last_pts)
            .map_or(0.0, |(first, last)| PtsUnwrapper::to_seconds(last - first));

        let programs = self
            .pat
            .iter()
            .filter_map(|(&program_number, &pmt_pid)| {
                let pmt = self.pmts.get(&pmt_pid)?;
                Some(ProgramInfo {
                    program_number,
                    pmt_pid,
                    pcr_pid: pmt.pcr_pid,
                    streams: pmt
                        .streams
                        .iter()
                        .map(|es| StreamInfo {
                            pid: es.pid,
                            stream_type: es.stream_type,
                            kind: StreamKind::resolve(es.stream_type, es.component_tag),
                            component_tag: es.component_tag,
                        })
                        .collect(),
                })
            })
            .collect();

        TsInfo {
            layout: self.layout,
            packet_count: self.packet_count,
            file_size,
            duration_seconds,
            programs,
            // The first format observed is the one at the head of the
            // recording; format_changes holds everything after it.
            video_format: self.initial_format,
            format_changes: self.format_changes,
            stats: self.stats,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::SYNC_BYTE;

    struct Builder {
        packets: Vec<u8>,
        cc: HashMap<u16, u8>,
    }

    impl Builder {
        fn new() -> Self {
            Self {
                packets: Vec::new(),
                cc: HashMap::new(),
            }
        }

        /// Append one packet. `psi` adds the pointer field that section
        /// payloads require and PES payloads must not have.
        fn push(&mut self, pid: u16, psi: bool, payload: &[u8]) -> &mut Self {
            let counter = self.cc.entry(pid).or_insert(0);
            let mut p = vec![0xFFu8; 188];
            p[0] = SYNC_BYTE;
            p[1] = ((pid >> 8) as u8) | 0x40; // payload_unit_start
            p[2] = (pid & 0xFF) as u8;
            p[3] = 0x10 | (*counter & 0x0F);
            *counter = counter.wrapping_add(1);
            let mut cursor = 4;
            if psi {
                p[cursor] = 0; // PSI pointer field
                cursor += 1;
            }
            let take = payload.len().min(188 - cursor);
            assert_eq!(take, payload.len(), "test payloads must fit one packet");
            p[cursor..cursor + take].copy_from_slice(&payload[..take]);
            self.packets.extend_from_slice(&p);
            self
        }
    }

    fn psi_section(table_id: u8, body: &[u8]) -> Vec<u8> {
        let mut s = vec![table_id, 0, 0];
        s.extend_from_slice(&[0x00, 0x01, 0xC1, 0x00, 0x00]);
        s.extend_from_slice(body);
        s.extend_from_slice(&[0, 0, 0, 0]);
        let length = s.len() - 3;
        s[1] = 0xB0 | ((length >> 8) as u8);
        s[2] = (length & 0xFF) as u8;
        s
    }

    /// A PES packet carrying an MPEG-2 sequence header for the given geometry.
    fn video_pes(pts: u64, width: u32, height: u32) -> Vec<u8> {
        let mut pes = vec![0x00, 0x00, 0x01, 0xE0, 0x00, 0x00, 0x80, 0x80, 5];
        pes.extend_from_slice(&[
            0x21 | (((pts >> 29) as u8) & 0x0E),
            ((pts >> 22) & 0xFF) as u8,
            (((pts >> 14) as u8) & 0xFE) | 0x01,
            ((pts >> 7) & 0xFF) as u8,
            (((pts << 1) as u8) & 0xFE) | 0x01,
        ]);
        pes.extend_from_slice(&[0x00, 0x00, 0x01, 0xB3]);
        pes.extend_from_slice(&[
            (width >> 4) as u8,
            (((width & 0x0F) << 4) | (height >> 8)) as u8,
            (height & 0xFF) as u8,
            0x24,
            0x00,
            0x00,
            0x00,
            0x00,
        ]);
        pes
    }

    fn build_stream(changes: &[(u64, u32, u32)]) -> Vec<u8> {
        let pat = psi_section(0x00, &[0x04, 0x00, 0xE1, 0x00]);
        let pmt = psi_section(
            0x02,
            &[0xE2, 0x00, 0x00, 0x00, 0x02, 0xE2, 0x00, 0x00, 0x00],
        );

        let mut builder = Builder::new();
        builder.push(0x0000, true, &pat);
        builder.push(0x0100, true, &pmt);
        for &(pts, width, height) in changes {
            builder.push(0x0200, false, &video_pes(pts, width, height));
        }
        // The final access unit is flushed by `finish`, so no trailing packet
        // is needed — and adding one would corrupt the PTS span.
        builder.packets
    }

    #[test]
    fn scans_programs_streams_and_duration() {
        let stream = build_stream(&[(90_000, 1440, 1080), (90_000 * 11, 1440, 1080)]);
        let size = stream.len() as u64;
        let info = scan(std::io::Cursor::new(stream), size).expect("scan");

        assert_eq!(info.layout, PacketLayout::Ts188);
        let program = info.primary_program().expect("a program with video");
        assert_eq!(program.program_number, 0x0400);
        assert_eq!(program.pcr_pid, 0x0200);
        assert_eq!(program.video().expect("video stream").pid, 0x0200);

        assert!(
            (info.duration_seconds - 10.0).abs() < 0.01,
            "duration was {}",
            info.duration_seconds
        );
        assert_eq!(
            info.video_format.map(|f| (f.width, f.height)),
            Some((1440, 1080))
        );
        assert!(!info.requires_split());
    }

    #[test]
    fn detects_a_mid_recording_resolution_change() {
        let stream = build_stream(&[
            (90_000, 1440, 1080),
            (90_000 * 6, 1440, 1080),
            (90_000 * 11, 720, 480),
        ]);
        let size = stream.len() as u64;
        let info = scan(std::io::Cursor::new(stream), size).expect("scan");

        assert!(
            info.requires_split(),
            "a 1080->480 change must force a split"
        );
        let change = info
            .format_changes
            .iter()
            .find(|c| c.format.width == 720)
            .expect("the SD change");
        assert!(
            (change.seconds - 10.0).abs() < 0.05,
            "change at {}s",
            change.seconds
        );
    }

    #[test]
    fn counts_scrambled_packets_and_flags_a_cas_failure() {
        let mut stream = build_stream(&[(90_000, 1440, 1080)]);
        // Mark every video packet scrambled, as a failed CAS would.
        for chunk in stream.chunks_mut(188) {
            if chunk[1] & 0x1F == 0x02 && chunk[2] == 0x00 {
                chunk[3] |= 0x80;
            }
        }
        let size = stream.len() as u64;
        let info = scan(std::io::Cursor::new(stream), size).expect("scan");
        assert!(info.stats.scrambled_packets > 0);
        assert!(info.stats.is_severely_damaged(info.packet_count));
    }

    #[test]
    fn rejects_input_that_is_not_a_transport_stream() {
        let junk = vec![0x00u8; 4096];
        assert!(matches!(
            scan(std::io::Cursor::new(junk), 4096),
            Err(Error::NoSync)
        ));
    }
}
