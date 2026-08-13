//! Video sequence header parsing.
//!
//! Asaborake needs the picture geometry and frame rate, and needs to notice
//! when either changes mid-recording. Japanese broadcast switches resolution
//! at programme boundaries often enough that treating a recording as one fixed
//! format produces broken output — a sub-channel dropping from 1440x1080 to
//! 720x480 for a shopping block is routine.

/// Picture geometry and frame rate as signalled by the video stream.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VideoFormat {
    /// Coded picture width in pixels.
    pub width: u32,
    /// Coded picture height in pixels.
    pub height: u32,
    /// Frame rate as an exact rational, e.g. 30000/1001.
    pub frame_rate: (u32, u32),
    /// Whether the stream signals interlaced coding.
    pub interlaced: bool,
}

impl VideoFormat {
    /// Frame rate as a floating-point value, for display and rough maths.
    #[must_use]
    pub fn fps(&self) -> f64 {
        if self.frame_rate.1 == 0 {
            return 0.0;
        }
        f64::from(self.frame_rate.0) / f64::from(self.frame_rate.1)
    }

    /// Whether two formats differ in a way that requires splitting the output.
    ///
    /// Frame-rate changes alone are tolerated because the encoder resamples
    /// them; a geometry change is what actually forces a new output file.
    #[must_use]
    pub fn requires_split(&self, other: &Self) -> bool {
        self.width != other.width || self.height != other.height
    }
}

/// MPEG-2 `frame_rate_code` to an exact rational, as specified in 13818-2.
const MPEG2_FRAME_RATES: [(u32, u32); 9] = [
    (0, 1), // forbidden
    (24000, 1001),
    (24, 1),
    (25, 1),
    (30000, 1001),
    (30, 1),
    (50, 1),
    (60000, 1001),
    (60, 1),
];

/// Locate and parse the first MPEG-2 sequence header in a buffer.
#[must_use]
pub fn parse_mpeg2_sequence_header(data: &[u8]) -> Option<VideoFormat> {
    let position = find_start_code(data, 0xB3)?;
    let b = data.get(position..position + 8)?;

    let width = (u32::from(b[0]) << 4) | (u32::from(b[1]) >> 4);
    let height = ((u32::from(b[1]) & 0x0F) << 8) | u32::from(b[2]);
    let frame_rate_code = usize::from(b[3] & 0x0F);
    let frame_rate = MPEG2_FRAME_RATES
        .get(frame_rate_code)
        .copied()
        .unwrap_or((0, 1));

    if width == 0 || height == 0 || frame_rate.0 == 0 {
        return None;
    }

    // progressive_sequence lives in the sequence extension (start code 0xB5,
    // extension id 0x1). Its absence means MPEG-1 semantics: progressive.
    let interlaced = find_sequence_extension(data).is_some_and(|ext| ext & 0x08 == 0);

    Some(VideoFormat {
        width,
        height,
        frame_rate,
        interlaced,
    })
}

/// Find the payload start of a start code with the given identifier.
fn find_start_code(data: &[u8], code: u8) -> Option<usize> {
    data.windows(4)
        .position(|w| w[0] == 0 && w[1] == 0 && w[2] == 1 && w[3] == code)
        .map(|p| p + 4)
}

/// Return the byte holding `progressive_sequence` from the sequence extension.
fn find_sequence_extension(data: &[u8]) -> Option<u8> {
    let mut cursor = 0;
    while let Some(found) = find_start_code(&data[cursor..], 0xB5) {
        let position = cursor + found;
        let byte = *data.get(position)?;
        // Extension start code identifier 0b0001 marks the sequence extension.
        if byte >> 4 == 0x1 {
            return data.get(position + 1).copied();
        }
        cursor = position;
    }
    None
}

/// Locate and parse the first H.264 sequence parameter set in a buffer.
#[must_use]
pub fn parse_h264_sps(data: &[u8]) -> Option<VideoFormat> {
    let sps = find_nal(data, 7)?;
    let rbsp = remove_emulation_prevention(sps);
    let mut reader = BitReader::new(&rbsp);

    let profile_idc = reader.bits(8)? as u8;
    reader.bits(8)?; // constraint flags + reserved
    reader.bits(8)?; // level_idc
    reader.unsigned_exp_golomb()?; // seq_parameter_set_id

    let mut chroma_format_idc = 1u32;
    if matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        chroma_format_idc = reader.unsigned_exp_golomb()?;
        if chroma_format_idc == 3 {
            reader.bits(1)?; // separate_colour_plane_flag
        }
        reader.unsigned_exp_golomb()?; // bit_depth_luma_minus8
        reader.unsigned_exp_golomb()?; // bit_depth_chroma_minus8
        reader.bits(1)?; // qpprime_y_zero_transform_bypass_flag
        if reader.bits(1)? == 1 {
            // seq_scaling_matrix_present_flag
            let count = if chroma_format_idc == 3 { 12 } else { 8 };
            for i in 0..count {
                if reader.bits(1)? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    skip_scaling_list(&mut reader, size)?;
                }
            }
        }
    }

    reader.unsigned_exp_golomb()?; // log2_max_frame_num_minus4
    let pic_order_cnt_type = reader.unsigned_exp_golomb()?;
    if pic_order_cnt_type == 0 {
        reader.unsigned_exp_golomb()?; // log2_max_pic_order_cnt_lsb_minus4
    } else if pic_order_cnt_type == 1 {
        reader.bits(1)?; // delta_pic_order_always_zero_flag
        reader.signed_exp_golomb()?; // offset_for_non_ref_pic
        reader.signed_exp_golomb()?; // offset_for_top_to_bottom_field
        let cycle = reader.unsigned_exp_golomb()?;
        for _ in 0..cycle {
            reader.signed_exp_golomb()?;
        }
    }

    reader.unsigned_exp_golomb()?; // max_num_ref_frames
    reader.bits(1)?; // gaps_in_frame_num_value_allowed_flag

    let width_mbs = reader.unsigned_exp_golomb()? + 1;
    let height_map_units = reader.unsigned_exp_golomb()? + 1;
    let frame_mbs_only = reader.bits(1)?;
    if frame_mbs_only == 0 {
        reader.bits(1)?; // mb_adaptive_frame_field_flag
    }
    reader.bits(1)?; // direct_8x8_inference_flag

    let mut crop = (0u32, 0u32, 0u32, 0u32);
    if reader.bits(1)? == 1 {
        crop = (
            reader.unsigned_exp_golomb()?,
            reader.unsigned_exp_golomb()?,
            reader.unsigned_exp_golomb()?,
            reader.unsigned_exp_golomb()?,
        );
    }

    // Crop offsets are expressed in chroma samples, so their pixel weight
    // depends on the subsampling in use.
    let (sub_width, sub_height) = match chroma_format_idc {
        0 | 3 => (1, 1), // monochrome and 4:4:4 are unsubsampled
        2 => (2, 1),     // 4:2:2
        _ => (2, 2),     // 4:2:0
    };
    let crop_unit_x = sub_width;
    let crop_unit_y = sub_height * (2 - u32::from(frame_mbs_only == 1));

    let width = width_mbs * 16 - (crop.0 + crop.1) * crop_unit_x;
    let height = (2 - u32::from(frame_mbs_only == 1)) * height_map_units * 16
        - (crop.2 + crop.3) * crop_unit_y;

    // Frame rate lives in the optional VUI timing info; fall back to the
    // Japanese broadcast norm when it is absent rather than reporting zero.
    let frame_rate = parse_vui_frame_rate(&mut reader).unwrap_or((30000, 1001));

    if width == 0 || height == 0 {
        return None;
    }

    Some(VideoFormat {
        width,
        height,
        frame_rate,
        interlaced: frame_mbs_only == 0,
    })
}

/// Read the VUI far enough to recover `time_scale` / `num_units_in_tick`.
fn parse_vui_frame_rate(reader: &mut BitReader<'_>) -> Option<(u32, u32)> {
    if reader.bits(1)? != 1 {
        return None; // vui_parameters_present_flag
    }
    if reader.bits(1)? == 1 {
        // aspect_ratio_info_present_flag
        let aspect_ratio_idc = reader.bits(8)?;
        if aspect_ratio_idc == 255 {
            reader.bits(16)?; // sar_width
            reader.bits(16)?; // sar_height
        }
    }
    if reader.bits(1)? == 1 {
        reader.bits(1)?; // overscan_appropriate_flag
    }
    if reader.bits(1)? == 1 {
        // video_signal_type_present_flag
        reader.bits(3)?; // video_format
        reader.bits(1)?; // video_full_range_flag
        if reader.bits(1)? == 1 {
            reader.bits(24)?; // colour description
        }
    }
    if reader.bits(1)? == 1 {
        // chroma_loc_info_present_flag
        reader.unsigned_exp_golomb()?;
        reader.unsigned_exp_golomb()?;
    }
    if reader.bits(1)? != 1 {
        return None; // timing_info_present_flag
    }
    let num_units_in_tick = reader.bits(32)? as u32;
    let time_scale = reader.bits(32)? as u32;
    if num_units_in_tick == 0 || time_scale == 0 {
        return None;
    }
    // time_scale ticks per num_units_in_tick covers a *field* period, so a
    // frame takes two of them.
    Some(reduce(time_scale, num_units_in_tick * 2))
}

fn skip_scaling_list(reader: &mut BitReader<'_>, size: usize) -> Option<()> {
    let mut last = 8i32;
    let mut next = 8i32;
    for _ in 0..size {
        if next != 0 {
            let delta = reader.signed_exp_golomb()?;
            next = (last + delta + 256) % 256;
        }
        last = if next == 0 { last } else { next };
    }
    Some(())
}

/// Reduce a rational to lowest terms.
fn reduce(numerator: u32, denominator: u32) -> (u32, u32) {
    let divisor = gcd(numerator, denominator);
    if divisor == 0 {
        return (numerator, denominator);
    }
    (numerator / divisor, denominator / divisor)
}

const fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// Find the payload of the first NAL unit of the requested type.
fn find_nal(data: &[u8], nal_type: u8) -> Option<&[u8]> {
    let mut i = 0;
    while i + 4 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            let header = data[i + 3];
            let start = i + 4;
            if header & 0x1F == nal_type {
                let end = find_next_start_code(data, start).unwrap_or(data.len());
                return data.get(start..end);
            }
            i = start;
        } else {
            i += 1;
        }
    }
    None
}

fn find_next_start_code(data: &[u8], from: usize) -> Option<usize> {
    data.get(from..)?
        .windows(3)
        .position(|w| w[0] == 0 && w[1] == 0 && w[2] == 1)
        .map(|p| from + p)
}

/// Strip the 0x03 bytes H.264 inserts to prevent accidental start codes.
fn remove_emulation_prevention(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut zeros = 0usize;
    for &byte in data {
        if zeros >= 2 && byte == 0x03 {
            zeros = 0;
            continue;
        }
        if byte == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
        out.push(byte);
    }
    out
}

/// Big-endian bit reader with the exp-Golomb decoding H.264 needs.
struct BitReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> BitReader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.position / 8)?;
        let shift = 7 - (self.position % 8);
        self.position += 1;
        Some(u32::from((byte >> shift) & 1))
    }

    fn bits(&mut self, count: usize) -> Option<u64> {
        let mut value = 0u64;
        for _ in 0..count {
            value = (value << 1) | u64::from(self.bit()?);
        }
        Some(value)
    }

    fn unsigned_exp_golomb(&mut self) -> Option<u32> {
        let mut leading = 0usize;
        while self.bit()? == 0 {
            leading += 1;
            // A run this long means the buffer is not a valid SPS; bail rather
            // than looping to the end of a corrupt payload.
            if leading > 32 {
                return None;
            }
        }
        if leading == 0 {
            return Some(0);
        }
        let rest = self.bits(leading)? as u32;
        Some((1u32 << leading) - 1 + rest)
    }

    fn signed_exp_golomb(&mut self) -> Option<i32> {
        // The unsigned code can reach 2^32-1, so the mapping is done in i64
        // and only then narrowed; a value that does not fit means the buffer
        // is not a valid parameter set.
        let value = i64::from(self.unsigned_exp_golomb()?);
        // The standard mapping: even codes are negative, odd codes positive.
        // `value` is non-negative here, so `(value + 1) / 2` is exact.
        let signed = if value % 2 == 0 {
            -(value / 2)
        } else {
            (value + 1) / 2
        };
        i32::try_from(signed).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mpeg2_sequence_header_for_hd_broadcast() {
        // 1440x1080, frame_rate_code 4 (30000/1001).
        let mut data = vec![0x00, 0x00, 0x01, 0xB3];
        data.extend_from_slice(&[0x5A, 0x04, 0x38, 0x24, 0x00, 0x00, 0x00, 0x00]);
        let format = parse_mpeg2_sequence_header(&data).expect("sequence header");
        assert_eq!(format.width, 1440);
        assert_eq!(format.height, 1080);
        assert_eq!(format.frame_rate, (30000, 1001));
        assert!((format.fps() - 29.97).abs() < 0.01);
    }

    #[test]
    fn parses_mpeg2_sd_sequence_header() {
        // 720x480.
        let mut data = vec![0x00, 0x00, 0x01, 0xB3];
        data.extend_from_slice(&[0x2D, 0x01, 0xE0, 0x24, 0x00, 0x00, 0x00, 0x00]);
        let format = parse_mpeg2_sequence_header(&data).expect("sequence header");
        assert_eq!((format.width, format.height), (720, 480));
    }

    #[test]
    fn geometry_change_forces_a_split_but_frame_rate_alone_does_not() {
        let hd = VideoFormat {
            width: 1440,
            height: 1080,
            frame_rate: (30000, 1001),
            interlaced: true,
        };
        let sd = VideoFormat {
            width: 720,
            height: 480,
            ..hd
        };
        let hd_60 = VideoFormat {
            frame_rate: (60000, 1001),
            ..hd
        };
        assert!(hd.requires_split(&sd));
        assert!(!hd.requires_split(&hd_60));
    }

    #[test]
    fn strips_emulation_prevention_bytes() {
        assert_eq!(
            remove_emulation_prevention(&[0x00, 0x00, 0x03, 0x01, 0x00, 0x00, 0x03, 0x02]),
            vec![0x00, 0x00, 0x01, 0x00, 0x00, 0x02]
        );
    }

    #[test]
    fn exp_golomb_round_trips_known_codes() {
        // Codes 1, 010, 011, 00100 decode to 0, 1, 2, 3.
        let mut reader = BitReader::new(&[0b1010_0110, 0b0100_0000]);
        assert_eq!(reader.unsigned_exp_golomb(), Some(0));
        assert_eq!(reader.unsigned_exp_golomb(), Some(1));
        assert_eq!(reader.unsigned_exp_golomb(), Some(2));
        assert_eq!(reader.unsigned_exp_golomb(), Some(3));
    }

    #[test]
    fn parses_h264_sps_geometry() {
        // A real 1920x1080 High profile SPS emitted by x264.
        let sps = [
            0x00u8, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x28, 0xAC, 0xD9, 0x40, 0x78, 0x02, 0x27,
            0xE5, 0xC0, 0x44, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00, 0x00, 0x03, 0x00, 0xF0, 0x3C,
            0x60, 0xC6, 0x58,
        ];
        let format = parse_h264_sps(&sps).expect("sps");
        assert_eq!((format.width, format.height), (1920, 1080));
    }
}
