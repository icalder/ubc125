//! WebM/EBML byte-stream segmenter for live audio capture.
//!
//! Splits a raw WebM byte stream (the native muxer's output, or any
//! `UBC125_AUDIO_CMD` source) into
//! the units the browser MSE path needs:
//!
//! - one **init segment**: the EBML header, the (open, unknown-size) `Segment`
//!   header, `Info`, and `Tracks`; and
//! - one **media segment** per complete `Cluster` element.
//!
//! Pipe reads have no relationship to EBML boundaries, so all parsing is
//! incremental: bytes may be fed one at a time or in arbitrary read sizes.
//! Malformed input (non-EBML prefix, unknown-size children, missing `Tracks`,
//! oversized clusters) is rejected with an error instead of being emitted, so
//! a bad capture can never poison client MSE buffers.

use std::error::Error;
use std::fmt;

/// Default maximum size of a single emitted media segment (one WebM cluster).
pub const DEFAULT_MAX_SEGMENT_SIZE: usize = 64 * 1024;

const EBML_HEADER_ID: u32 = 0x1A45_DFA3;
const SEGMENT_ID: u32 = 0x1853_8067;
const INFO_ID: u32 = 0x1549_A966;
const TRACKS_ID: u32 = 0x1654_AE6B;
/// ffmpeg's webm muxer emits a SeekHead before Info/Tracks; it is valid WebM
/// and small, so it is folded into the init segment.
const SEEK_HEAD_ID: u32 = 0x114D_9B74;
/// EBML Void element. ffmpeg's matroska muxer reserves the SeekHead area with
/// a Void and, on non-seekable output (our pipe), leaves it in the stream.
const VOID_ID: u32 = 0xEC;
const CLUSTER_ID: u32 = 0x1F43_B675;

/// A complete WebM unit ready for broadcast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// EBML header + open Segment header + Info + Tracks.
    Init(Vec<u8>),
    /// One complete Cluster element (id + size + payload).
    Media(Vec<u8>),
}

/// Error for malformed or oversized WebM input.
#[derive(Debug, PartialEq, Eq)]
pub enum WebmError {
    Malformed(String),
    /// A cluster larger than the configured `max_segment_size`.
    Oversized {
        max: usize,
        size: u64,
    },
}

impl fmt::Display for WebmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(msg) => write!(f, "malformed WebM: {msg}"),
            Self::Oversized { max, size } => {
                write!(f, "cluster of {size} bytes exceeds limit of {max} bytes")
            }
        }
    }
}

impl Error for WebmError {}

/// Duration in ms of one complete WebM cluster: the span of its
/// `SimpleBlock` relative timecodes plus one frame (the muxer's blocks
/// carry a 2-byte big-endian relative timecode, `src/audio/webm_mux.rs`
/// `add_block`). Used by the pump's B10 counters; returns `None` when the
/// bytes are not a single parseable cluster with at least one block.
pub fn cluster_duration_ms(bytes: &[u8]) -> Option<u64> {
    const FRAME_MS: u64 = 20;
    let (id, id_len) = read_vint_id(bytes).ok()??;
    if id != CLUSTER_ID {
        return None;
    }
    let (size, size_len) = read_vint_size(&bytes[id_len..]).ok()??;
    let size = size? as usize;
    let body = bytes.get(id_len + size_len..)?.get(..size)?;
    // First child must be the mandatory Timecode; its value is skipped —
    // the duration comes from the blocks' relative timecodes.
    let (id, id_len) = read_vint_id(body).ok()??;
    if id != 0xE7 {
        return None;
    }
    let (size, size_len) = read_vint_size(&body[id_len..]).ok()??;
    let body = body.get(id_len + size_len + size? as usize..)?;
    let mut first_rel: Option<u16> = None;
    let mut last_rel: Option<u16> = None;
    let mut rest = body;
    while !rest.is_empty() {
        let (id, id_len) = read_vint_id(rest).ok()??;
        if id != 0xA3 {
            return None;
        }
        let (size, size_len) = read_vint_size(&rest[id_len..]).ok()??;
        let block = rest.get(id_len + size_len..)?.get(..size? as usize)?;
        rest = &rest[id_len + size_len + size? as usize..];
        // Block data: track vint + 2-byte BE relative timecode + flags +
        // payload (the muxer's fixed layout; track 1 encodes as 0x81).
        if block.len() < 4 || block[0] != 0x81 {
            return None;
        }
        let rel = u16::from_be_bytes([block[1], block[2]]);
        first_rel.get_or_insert(rel);
        last_rel = Some(rel);
    }
    let (first_rel, last_rel) = (first_rel?, last_rel?);
    Some(u64::from(last_rel - first_rel) + FRAME_MS)
}

#[derive(Debug, Clone, Copy)]
enum State {
    AwaitingEbml,
    AwaitingSegment,
    InSegment { have_info: bool, have_tracks: bool },
    Streaming,
}

/// Incremental WebM byte-stream segmenter.
#[derive(Debug)]
pub struct WebmSegmenter {
    buf: Vec<u8>,
    pos: usize,
    max_segment_size: usize,
    state: State,
    /// Raw bytes of the init segment under construction (EBML header first,
    /// then Segment header, Info, Tracks in order of appearance).
    init_buf: Vec<u8>,
}

impl WebmSegmenter {
    /// Create a segmenter that rejects clusters larger than `max_segment_size`.
    pub fn new(max_segment_size: usize) -> Self {
        Self {
            buf: Vec::new(),
            pos: 0,
            max_segment_size,
            state: State::AwaitingEbml,
            init_buf: Vec::new(),
        }
    }

    /// Feed raw bytes and return any segments completed by this feed.
    /// Safe to call with any non-empty or empty slice, any number of times.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Segment>, WebmError> {
        if !bytes.is_empty() {
            self.compact();
            self.buf.extend_from_slice(bytes);
        }
        let mut segments = Vec::new();
        loop {
            let progressed = match self.state {
                State::AwaitingEbml => self.parse_ebml_header(&mut segments)?,
                State::AwaitingSegment => self.parse_segment_header()?,
                State::InSegment { .. } => self.parse_segment_child(&mut segments)?,
                State::Streaming => self.parse_cluster(&mut segments)?,
            };
            if !progressed {
                return Ok(segments);
            }
        }
    }

    /// Drop consumed bytes so the buffer does not grow without bound.
    fn compact(&mut self) {
        if self.pos > 0 && self.buf.len() - self.pos < self.buf.len() / 2 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }

    fn rest(&self) -> &[u8] {
        &self.buf[self.pos..]
    }

    fn parse_ebml_header(&mut self, _segments: &mut Vec<Segment>) -> Result<bool, WebmError> {
        let rest = self.rest();
        let Some((id, id_len)) = read_vint_id(rest)? else {
            return Ok(false);
        };
        if id != EBML_HEADER_ID {
            return Err(WebmError::Malformed(format!(
                "expected EBML header element 0x{EBML_HEADER_ID:08X}, found 0x{id:08X}"
            )));
        }
        let Some((size, size_len)) = read_vint_size(&rest[id_len..])? else {
            return Ok(false);
        };
        let size =
            size.ok_or_else(|| WebmError::Malformed("EBML header with unknown size".into()))?;
        if size as usize > self.max_segment_size {
            return Err(WebmError::Oversized {
                max: self.max_segment_size,
                size,
            });
        }
        let total = id_len + size_len + size as usize;
        if rest.len() < total {
            return Ok(false);
        }
        let end = self.pos + total;
        self.init_buf.extend_from_slice(&self.buf[self.pos..end]);
        self.pos = end;
        self.state = State::AwaitingSegment;
        Ok(true)
    }

    fn parse_segment_header(&mut self) -> Result<bool, WebmError> {
        let rest = self.rest();
        let Some((id, id_len)) = read_vint_id(rest)? else {
            return Ok(false);
        };
        if id != SEGMENT_ID {
            return Err(WebmError::Malformed(format!(
                "expected Segment element after EBML header, found 0x{id:08X}"
            )));
        }
        let Some((size, size_len)) = read_vint_size(&rest[id_len..])? else {
            return Ok(false);
        };
        if size.is_some() {
            return Err(WebmError::Malformed(
                "Segment element with known size cannot be streamed".into(),
            ));
        }
        // Keep the Segment element open: record only its id + unknown-size header.
        let header_len = id_len + size_len;
        self.init_buf
            .extend_from_slice(&self.buf[self.pos..self.pos + header_len]);
        self.pos += header_len;
        self.state = State::InSegment {
            have_info: false,
            have_tracks: false,
        };
        Ok(true)
    }

    fn parse_segment_child(&mut self, segments: &mut Vec<Segment>) -> Result<bool, WebmError> {
        let rest = self.rest();
        let Some((id, id_len)) = read_vint_id(rest)? else {
            return Ok(false);
        };
        let Some((size, size_len)) = read_vint_size(&rest[id_len..])? else {
            return Ok(false);
        };
        match self.state {
            State::InSegment {
                have_info,
                have_tracks,
            } => match id {
                CLUSTER_ID => {
                    if !have_info || !have_tracks {
                        return Err(WebmError::Malformed(
                            "Cluster before Info/Tracks (init segment incomplete)".into(),
                        ));
                    }
                    let consumed = self.take_cluster(id_len, size, size_len, segments)?;
                    if !consumed {
                        return Ok(false);
                    }
                    self.state = State::Streaming;
                    Ok(true)
                }
                INFO_ID if !have_info => {
                    let size =
                        size.ok_or_else(|| WebmError::Malformed("Info with unknown size".into()))?;
                    if !self.take_init_child(id_len, size, size_len)? {
                        return Ok(false);
                    }
                    self.state = State::InSegment {
                        have_info: true,
                        have_tracks,
                    };
                    Ok(true)
                }
                SEEK_HEAD_ID => {
                    let size = size
                        .ok_or_else(|| WebmError::Malformed("SeekHead with unknown size".into()))?;
                    if !self.take_init_child(id_len, size, size_len)? {
                        return Ok(false);
                    }
                    // State unchanged; the SeekHead is folded into the init bytes.
                    Ok(true)
                }
                VOID_ID => {
                    let size =
                        size.ok_or_else(|| WebmError::Malformed("Void with unknown size".into()))?;
                    if size as usize > self.max_segment_size {
                        return Err(WebmError::Oversized {
                            max: self.max_segment_size,
                            size,
                        });
                    }
                    let total = id_len + size_len + size as usize;
                    if self.rest().len() < total {
                        return Ok(false);
                    }
                    // The Void is reserved space; skip it entirely.
                    self.pos += total;
                    Ok(true)
                }
                TRACKS_ID if !have_tracks => {
                    let size = size
                        .ok_or_else(|| WebmError::Malformed("Tracks with unknown size".into()))?;
                    if !self.take_init_child(id_len, size, size_len)? {
                        return Ok(false);
                    }
                    let init = std::mem::take(&mut self.init_buf);
                    segments.push(Segment::Init(init));
                    self.state = State::InSegment {
                        have_info,
                        have_tracks: true,
                    };
                    Ok(true)
                }
                other if have_info && have_tracks => {
                    // ffmpeg writes metadata elements (e.g. Tags with the
                    // ENCODER tag) between Tracks and the first Cluster.
                    // Skip any top-level element once the init is complete.
                    let size = size.ok_or_else(|| {
                        WebmError::Malformed(format!(
                            "element 0x{other:08X} with unknown size before first Cluster"
                        ))
                    })?;
                    if size as usize > self.max_segment_size {
                        return Err(WebmError::Oversized {
                            max: self.max_segment_size,
                            size,
                        });
                    }
                    let total = id_len + size_len + size as usize;
                    if self.rest().len() < total {
                        return Ok(false);
                    }
                    self.pos += total;
                    Ok(true)
                }
                other => Err(WebmError::Malformed(format!(
                    "unexpected element 0x{other:08X} in Segment before init complete"
                ))),
            },
            _ => unreachable!("parse_segment_child called outside InSegment"),
        }
    }

    fn parse_cluster(&mut self, segments: &mut Vec<Segment>) -> Result<bool, WebmError> {
        let rest = self.rest();
        let Some((id, id_len)) = read_vint_id(rest)? else {
            return Ok(false);
        };
        if id != CLUSTER_ID {
            return Err(WebmError::Malformed(format!(
                "expected Cluster in streaming state, found 0x{id:08X}"
            )));
        }
        let Some((size, size_len)) = read_vint_size(&rest[id_len..])? else {
            return Ok(false);
        };
        self.take_cluster(id_len, size, size_len, segments)
    }

    /// Consume a complete Cluster element as a media segment.
    fn take_cluster(
        &mut self,
        id_len: usize,
        size: Option<u64>,
        size_len: usize,
        segments: &mut Vec<Segment>,
    ) -> Result<bool, WebmError> {
        let rest = self.rest();
        let size = size.ok_or_else(|| WebmError::Malformed("Cluster with unknown size".into()))?;
        if size as usize > self.max_segment_size {
            return Err(WebmError::Oversized {
                max: self.max_segment_size,
                size,
            });
        }
        let total = id_len + size_len + size as usize;
        if rest.len() < total {
            return Ok(false);
        }
        let end = self.pos + total;
        segments.push(Segment::Media(self.buf[self.pos..end].to_vec()));
        self.pos = end;
        Ok(true)
    }

    /// Consume an Info/Tracks element into the init buffer.
    fn take_init_child(
        &mut self,
        id_len: usize,
        size: u64,
        size_len: usize,
    ) -> Result<bool, WebmError> {
        let rest = self.rest();
        if size as usize > self.max_segment_size {
            return Err(WebmError::Oversized {
                max: self.max_segment_size,
                size,
            });
        }
        let total = id_len + size_len + size as usize;
        if rest.len() < total {
            return Ok(false);
        }
        let end = self.pos + total;
        self.init_buf.extend_from_slice(&self.buf[self.pos..end]);
        self.pos = end;
        Ok(true)
    }
}

/// Parse a 1–4 byte EBML element id vint.
/// Returns `(id, byte_len)`, or `Ok(None)` if fewer bytes are available.
fn read_vint_id(bytes: &[u8]) -> Result<Option<(u32, usize)>, WebmError> {
    // An empty slice means no element has started yet, not a defect.
    let Some(first) = bytes.first() else {
        return Ok(None);
    };
    if *first == 0 {
        return Err(WebmError::Malformed("zero element id vint".into()));
    }
    // Element ids are raw vints: every bit is significant, length = first set
    // bit position (1-based), so len = leading_zeros + 1.
    let len = first.leading_zeros() as usize + 1;
    if len > 4 {
        return Err(WebmError::Malformed(
            "element id longer than 4 bytes".into(),
        ));
    }
    if bytes.len() < len {
        return Ok(None);
    }
    let mut id = 0u32;
    for &b in &bytes[..len] {
        id = (id << 8) | b as u32;
    }
    Ok(Some((id, len)))
}

/// Parse a 1–8 byte EBML size vint.
/// Returns `(size, byte_len)` where `size == None` means unknown (infinite)
/// size, or `Ok(None)` if fewer bytes are available.
fn read_vint_size(bytes: &[u8]) -> Result<Option<(Option<u64>, usize)>, WebmError> {
    // An empty slice means the size byte has not arrived yet, not a defect.
    let Some(first) = bytes.first() else {
        return Ok(None);
    };
    if *first == 0 {
        return Err(WebmError::Malformed("zero size vint".into()));
    }
    // len = position of the marker bit (1-based): 1, 2, 3, 4, 8-byte sizes.
    let len = first.leading_zeros() as usize + 1;
    if len > 8 {
        return Err(WebmError::Malformed("size vint longer than 8 bytes".into()));
    }
    if bytes.len() < len {
        return Ok(None);
    }
    // Continuation bytes may be zero (real muxers emit 4-byte sizes in
    // 0x10_00xxxx form), so no strict canonical check here.
    // Data bits: (9 - len) in the first byte + 8 per continuation byte,
    // i.e. 7 * len (7 / 14 / 28 / 56 for 1 / 2 / 4 / 8-byte vints).
    let data_bits = 7 * len;
    // Unknown size: every data bit set.
    let first_data_mask = if len < 8 { (1u8 << (8 - len)) - 1 } else { 0 };
    if first & first_data_mask == first_data_mask && bytes[1..len].iter().all(|&b| b == 0xFF) {
        return Ok(Some((None, len)));
    }
    let mut size = 0u64;
    for &b in &bytes[..len] {
        size = (size << 8) | b as u64;
    }
    let size_mask = if data_bits == 64 {
        u64::MAX
    } else {
        (1u64 << data_bits) - 1
    };
    Ok(Some((Some(size & size_mask), len)))
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    // Reuse the muxer's canonical id encoding instead of a second copy
    // (re-exported so the test module's glob import keeps working).
    pub(crate) use crate::audio::webm_mux::id_bytes;

    /// Minimal canonical vint size encoding for `value`.
    ///
    /// Widths: 1 byte (7 data bits), 2 (14), 4 (28), 8 (56); there is no
    /// 3- or 7-byte size vint. An all-1s data encoding means *unknown size*,
    /// so each width's top value (127, 16383, ...) steps up to the next
    /// wider form instead.
    pub(crate) fn vint_size(value: u64) -> Vec<u8> {
        assert!(
            value < 0x00FF_FFFF_FFFF_FFFE,
            "value too large for 8-byte vint (all-ones = unknown size)"
        );
        if value <= 0x7E {
            vec![0x80 | value as u8]
        } else if value <= 0x3FFE {
            vec![0x40 | ((value >> 8) as u8), value as u8]
        } else if value <= 0x0FFF_FFFE {
            vec![
                0x10 | ((value >> 24) as u8 & 0x0F),
                ((value >> 16) as u8 & 0xFF),
                ((value >> 8) as u8 & 0xFF),
                value as u8,
            ]
        } else {
            let b = value.to_be_bytes();
            vec![0x01, b[1], b[2], b[3], b[4], b[5], b[6], b[7]]
        }
    }

    pub(crate) fn unknown_size() -> Vec<u8> {
        vec![0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
    }

    /// Build a complete EBML element (id + size + payload).
    /// `size` of `None` encodes an unknown (infinite) size.
    pub(crate) fn element(id: u32, size: Option<u64>, payload: &[u8]) -> Vec<u8> {
        let mut out = id_bytes(id);
        match size {
            Some(s) => assert_eq!(s as usize, payload.len()),
            None => {}
        }
        out.extend(match size {
            Some(s) => vint_size(s),
            None => unknown_size(),
        });
        out.extend_from_slice(payload);
        out
    }

    /// A valid WebM stream: EBML header, open Segment, Info, Tracks, and
    /// `clusters` clusters. Returns (full_stream, init_bytes).
    pub fn build_fixture(clusters: usize) -> (Vec<u8>, Vec<u8>) {
        let ebml = element(EBML_HEADER_ID, Some(5), b"1.4.2");
        let segment_header = {
            let mut h = id_bytes(SEGMENT_ID);
            h.extend(unknown_size());
            h
        };
        let info = element(INFO_ID, Some(9), b"info-data");
        let tracks = element(TRACKS_ID, Some(16), b"tracks-data-here");
        let mut stream = Vec::new();
        stream.extend_from_slice(&ebml);
        stream.extend_from_slice(&segment_header);
        stream.extend_from_slice(&info);
        stream.extend_from_slice(&tracks);
        let init = stream.clone();
        for i in 0..clusters {
            let payload = format!("cluster-{i}-payload");
            stream.extend(element(
                CLUSTER_ID,
                Some(payload.len() as u64),
                payload.as_bytes(),
            ));
        }
        (stream, init)
    }

    pub(crate) fn expected_media(stream: &[u8], init_len: usize) -> Vec<Vec<u8>> {
        // Extract each trailing cluster element exactly.
        let mut media = Vec::new();
        let mut rest = &stream[init_len..];
        while !rest.is_empty() {
            let (id, id_len) = read_vint_id(rest).unwrap().unwrap();
            assert_eq!(id, CLUSTER_ID);
            let (size, size_len) = read_vint_size(&rest[id_len..]).unwrap().unwrap();
            let size = size.unwrap() as usize;
            media.push(rest[..id_len + size_len + size].to_vec());
            rest = &rest[id_len + size_len + size..];
        }
        media
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::webm::fixtures::*;
    use crate::audio::webm_mux::{OPUS_FRAME_MS, WebmMuxer};

    /// B10: the pump's duration counter must read a cluster's own blocks.
    /// 50 frames at the 60 ms default: 16 full clusters (60 ms) + a
    /// flushed 2-frame tail (40 ms).
    #[test]
    fn cluster_duration_reads_the_muxers_own_blocks() {
        let mut muxer = WebmMuxer::default();
        let mut clusters = Vec::new();
        for frame in 0..50u64 {
            let packet = [0xAB; 10];
            clusters.extend(muxer.add_block(frame * OPUS_FRAME_MS, &packet));
        }
        clusters.push(muxer.flush().unwrap());
        for (i, cluster) in clusters.iter().enumerate() {
            let expected = if i + 1 < clusters.len() { 60 } else { 40 };
            assert_eq!(
                cluster_duration_ms(cluster),
                Some(expected),
                "cluster {i} duration"
            );
        }
        // A non-cluster element is not a duration.
        assert_eq!(cluster_duration_ms(&[0x18, 0x53, 0x80, 0x67, 0x01]), None);
    }

    #[test]
    fn void_element_between_seekhead_and_info_is_skipped() {
        let mut stream = Vec::new();
        stream.extend(element(EBML_HEADER_ID, Some(5), b"1.4.2"));
        let mut segment_header = id_bytes(SEGMENT_ID);
        segment_header.extend(unknown_size());
        stream.extend_from_slice(&segment_header);
        stream.extend(element(SEEK_HEAD_ID, Some(13), b"seekhead-data"));
        // ffmpeg leaves a reserved-space Void (zeros) on non-seekable output.
        stream.extend(element(VOID_ID, Some(16), &vec![0u8; 16]));
        stream.extend(element(INFO_ID, Some(9), b"info-data"));
        stream.extend(element(TRACKS_ID, Some(16), b"tracks-data-here"));
        stream.extend(element(CLUSTER_ID, Some(4), b"clst"));
        let segments = feed_all(WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE), &stream);
        assert_eq!(segments.len(), 2);
        let Segment::Init(init) = &segments[0] else {
            panic!("expected init first")
        };
        assert!(!init.contains(&0xEC), "init must not contain the Void");
        assert!(matches!(segments[1], Segment::Media(_)));
    }

    #[test]
    fn top_level_element_between_tracks_and_first_cluster_is_skipped() {
        // ffmpeg emits a Tags element (ENCODER metadata) after Tracks.
        let tags_id = 0x1254_C367;
        let mut stream = Vec::new();
        stream.extend(element(EBML_HEADER_ID, Some(5), b"1.4.2"));
        let mut segment_header = id_bytes(SEGMENT_ID);
        segment_header.extend(unknown_size());
        stream.extend_from_slice(&segment_header);
        stream.extend(element(INFO_ID, Some(9), b"info-data"));
        stream.extend(element(TRACKS_ID, Some(16), b"tracks-data-here"));
        stream.extend(element(tags_id, Some(4), b"tags"));
        stream.extend(element(CLUSTER_ID, Some(4), b"clst"));
        let segments = feed_all(WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE), &stream);
        assert_eq!(segments.len(), 2);
        let Segment::Init(init) = &segments[0] else {
            panic!("expected init first")
        };
        assert!(
            !init
                .windows(4)
                .any(|w| u32::from_be_bytes(w.try_into().unwrap()) == tags_id)
        );
        assert!(matches!(segments[1], Segment::Media(_)));
    }

    #[test]
    fn vint_size_round_trips_across_widths() {
        // One value per width boundary: 1-byte (7 bits), 2-byte (14),
        // 4-byte (28), 8-byte (56). A mis-encoded width decodes to a
        // different value, so this catches first-byte/mask mistakes.
        // Includes the all-ones boundaries: 127/16383/2^28-1 must step up a
        // width (their same-width encoding is the unknown-size marker).
        let cases: [(u64, usize); 12] = [
            (1, 1),
            (126, 1),
            (127, 2),
            (200, 2),
            (0x3FFE, 2),
            (0x3FFF, 4),
            (0x4000, 4),
            (0x0ABCDE, 4),
            (300_000, 4),
            (0x0FFFF_FFE, 4),
            (0x0FFFF_FFF, 8),
            (0x1000_0000, 8),
        ];
        for (value, width) in cases {
            let enc = vint_size(value);
            assert_eq!(enc.len(), width, "encoding width for {value:#x}");
            let (decoded, len) = read_vint_size(&enc).unwrap().expect("complete vint");
            assert_eq!(len, width, "decoded width for {value:#x}");
            assert_eq!(decoded, Some(value), "round-trip for {value:#x}");
        }
    }

    fn feed_all(mut segmenter: WebmSegmenter, stream: &[u8]) -> Vec<Segment> {
        let out = segmenter.feed(stream).unwrap();
        assert_eq!(segmenter.feed(&[]).unwrap(), Vec::<Segment>::new());
        out
    }

    #[test]
    fn full_fixture_one_shot() {
        let (stream, init) = build_fixture(5);
        let segments = feed_all(WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE), &stream);
        let expected = expected_media(&stream, init.len());
        assert_eq!(
            segments,
            vec![Segment::Init(init)]
                .into_iter()
                .chain(expected.iter().cloned().map(Segment::Media))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn one_byte_at_a_time() {
        let (stream, init) = build_fixture(3);
        let mut segmenter = WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE);
        let mut segments = Vec::new();
        for b in &stream {
            segments.extend(segmenter.feed(std::slice::from_ref(b)).unwrap());
        }
        let expected = expected_media(&stream, init.len());
        assert_eq!(
            segments,
            vec![Segment::Init(init)]
                .into_iter()
                .chain(expected.iter().cloned().map(Segment::Media))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn awkward_chunk_boundaries() {
        let (stream, init) = build_fixture(4);
        // Chunk sizes chosen to split inside element ids, size vints, and
        // cluster payloads.
        let chunk_sizes = [1, 2, 3, 4, 5, 6, 7, 9, 11, 2, 13, 5, 4, 7, 3];
        let mut segmenter = WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE);
        let mut segments = Vec::new();
        let mut offset = 0;
        let mut i = 0;
        while offset < stream.len() {
            let n = chunk_sizes[i % chunk_sizes.len()].min(stream.len() - offset);
            segments.extend(segmenter.feed(&stream[offset..offset + n]).unwrap());
            offset += n;
            i += 1;
        }
        let expected = expected_media(&stream, init.len());
        assert_eq!(
            segments,
            vec![Segment::Init(init)]
                .into_iter()
                .chain(expected.iter().cloned().map(Segment::Media))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn seek_head_between_segment_and_info_is_folded_into_init() {
        let mut stream = Vec::new();
        stream.extend(element(EBML_HEADER_ID, Some(5), b"1.4.2"));
        let mut segment_header = id_bytes(SEGMENT_ID);
        segment_header.extend(unknown_size());
        stream.extend_from_slice(&segment_header);
        stream.extend(element(SEEK_HEAD_ID, Some(13), b"seekhead-data"));
        stream.extend(element(INFO_ID, Some(9), b"info-data"));
        stream.extend(element(TRACKS_ID, Some(16), b"tracks-data-here"));
        stream.extend(element(CLUSTER_ID, Some(4), b"clst"));
        let segments = feed_all(WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE), &stream);
        assert_eq!(segments.len(), 2);
        let Segment::Init(init) = &segments[0] else {
            panic!("expected init first")
        };
        assert!(
            init.windows(4).any(|w| w == &SEEK_HEAD_ID.to_be_bytes()),
            "init must contain the SeekHead element"
        );
        assert!(matches!(segments[1], Segment::Media(_)));
    }

    #[test]
    fn cluster_with_unknown_size_errors() {
        let (mut stream, init) = build_fixture(0);
        stream.extend(SEGMENT_ID.to_be_bytes());
        // Replace open segment with a fresh one is not valid; build a stream
        // where the first cluster has an unknown size instead.
        let mut bad = Vec::new();
        bad.extend(element(EBML_HEADER_ID, Some(5), b"1.4.2"));
        let mut segment_header = id_bytes(SEGMENT_ID);
        segment_header.extend(unknown_size());
        bad.extend_from_slice(&segment_header);
        bad.extend(element(INFO_ID, Some(9), b"info-data"));
        bad.extend(element(TRACKS_ID, Some(16), b"tracks-data-here"));
        let mut cluster = id_bytes(CLUSTER_ID);
        cluster.extend(unknown_size());
        cluster.extend_from_slice(b"some payload");
        bad.extend_from_slice(&cluster);
        let _ = init;
        let err = WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE)
            .feed(&bad)
            .unwrap_err();
        assert!(matches!(err, WebmError::Malformed(m) if m.contains("unknown size")));
    }

    #[test]
    fn missing_tracks_errors() {
        let mut stream = Vec::new();
        stream.extend(element(EBML_HEADER_ID, Some(5), b"1.4.2"));
        let mut segment_header = id_bytes(SEGMENT_ID);
        segment_header.extend(unknown_size());
        stream.extend_from_slice(&segment_header);
        stream.extend(element(INFO_ID, Some(9), b"info-data"));
        stream.extend(element(CLUSTER_ID, Some(4), b"clst"));
        let err = WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE)
            .feed(&stream)
            .unwrap_err();
        assert!(matches!(err, WebmError::Malformed(_)));
    }

    #[test]
    fn oversized_cluster_errors() {
        let limit = 64;
        let mut stream = Vec::new();
        stream.extend(element(EBML_HEADER_ID, Some(5), b"1.4.2"));
        let mut segment_header = id_bytes(SEGMENT_ID);
        segment_header.extend(unknown_size());
        stream.extend_from_slice(&segment_header);
        stream.extend(element(INFO_ID, Some(9), b"info-data"));
        stream.extend(element(TRACKS_ID, Some(16), b"tracks-data-here"));
        stream.extend(element(CLUSTER_ID, Some(65), &[0u8; 65]));
        let err = WebmSegmenter::new(limit).feed(&stream).unwrap_err();
        assert_eq!(
            err,
            WebmError::Oversized {
                max: limit,
                size: 65
            }
        );
    }

    #[test]
    fn non_ebml_prefix_errors() {
        let mut stream = b"this is not ebml at all".to_vec();
        stream.extend(element(EBML_HEADER_ID, Some(5), b"1.4.2"));
        let err = WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE)
            .feed(&stream)
            .unwrap_err();
        assert!(matches!(err, WebmError::Malformed(_)));
    }
}
