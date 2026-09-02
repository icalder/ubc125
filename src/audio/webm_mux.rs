//! WebM/EBML writer for live Opus audio.
//!
//! Emits the minimal WebM subset that Chromium's MSE accepts and the
//! [`WebmSegmenter`](crate::audio::webm::WebmSegmenter) parses:
//!
//! - an **init segment** (EBML header, open unknown-size `Segment`,
//!   `Info`, `Tracks`) produced once via [`WebmMuxer::init_segment`];
//! - **media segments**: one complete `Cluster` per flush, returned by
//!   [`WebmMuxer::add_block`] (when the flush limits are hit) or
//!   [`WebmMuxer::flush`] (at end of stream).
//!
//! The muxer is a pure streaming writer: the `Segment` element is emitted
//! with unknown (infinite) size and is never back-patched, so a file or
//! pipe gets the exact same byte shape. Cluster sizes are known when the
//! cluster closes, so each `Cluster` element is complete when returned.
//!
//! Chromium MSE constraints honoured here (see the research notes in
//! `NATIVE-AUDIO-PLAN.md` §1.4): `TimestampScale` is exactly 1 000 000,
//! `Info` precedes `Tracks`, `Cluster::Timecode` is absolute milliseconds,
//! every `SimpleBlock` carries the keyframe flag (Opus frames are all
//! independently decodable), and the `OpusHead` in `CodecPrivate` uses the
//! conventional pre-skip of 312 samples.

/// Opus sample rate (the codec's native rate; no resampling).
pub const OPUS_SAMPLE_RATE: u32 = 48_000;
/// Timecode in milliseconds for one Opus frame.
pub const OPUS_FRAME_MS: u64 = 20;
/// Default: close a cluster after this much audio (B1). 60 ms is three
/// 20 ms Opus frames — small enough to keep the whole pipeline under the
/// 150 ms target, large enough that the ~11-byte cluster header costs
/// ~0.2 kbps on a 24 kbps stream.
pub const DEFAULT_CLUSTER_TIME_MS: u64 = 60;
/// Close a cluster after this many payload bytes (matches
/// `DEFAULT_MAX_SEGMENT_SIZE`, so the segmenter's oversized check can
/// never fire on our own output).
const MAX_CLUSTER_BYTES: usize = 64 * 1024;
/// Default encoder bitrate in bits/s (matches the old ffmpeg `-b:a 24k`).
pub const DEFAULT_BITRATE_BPS: u32 = 24_000;

// EBML element ids.
const EBML_HEADER_ID: u32 = 0x1A45_DFA3;
const SEGMENT_ID: u32 = 0x1853_8067;
const INFO_ID: u32 = 0x1549_A966;
const TRACKS_ID: u32 = 0x1654_AE6B;
const CLUSTER_ID: u32 = 0x1F43_B675;
const SIMPLE_BLOCK_ID: u32 = 0xA3;
const TRACK_ENTRY_ID: u32 = 0xAE;
const AUDIO_ID: u32 = 0xE1;
const TIMECODE_ID: u32 = 0xE7;
const MUXING_APP_ID: u32 = 0x4D80;
const WRITING_APP_ID: u32 = 0x5741;
const NAME_ID: u32 = 0x536E;
const CODEC_ID_ID: u32 = 0x86;
const CODEC_PRIVATE_ID: u32 = 0x63_A2;
const CHANNELS_ID: u32 = 0x9F;
const SAMPLE_FREQ_ID: u32 = 0xB5;
const TIMESTAMP_SCALE_ID: u32 = 0x2AD7B1;
const TRACK_NUMBER_ID: u32 = 0xD7;
const TRACK_TYPE_ID: u32 = 0x83;
const FLAG_ENABLED_ID: u32 = 0xB9;
const FLAG_DEFAULT_ID: u32 = 0x88;

/// The single audio track number (kept <= 127 for the 1-byte SimpleBlock
/// encoding).
const TRACK_NUMBER: u8 = 1;
/// SimpleBlock flag: keyframe. Set on every block: each Opus frame is an
/// independent random-access point.
const FLAG_KEYFRAME: u8 = 0x80;

/// Muxes Opus packets into WebM: one init segment plus cluster-sized media
/// segments.
#[derive(Debug)]
pub struct WebmMuxer {
    /// Payload of the cluster under construction (SimpleBlocks).
    cluster: Vec<u8>,
    /// Whether a cluster is open.
    cluster_open: bool,
    /// Absolute timecode (ms) of the first block in the open cluster.
    cluster_start_ms: u64,
    /// Close a cluster after this much audio (B1; `--audio-cluster-ms`).
    max_cluster_time_ms: u64,
}

impl Default for WebmMuxer {
    fn default() -> Self {
        Self::with_cluster_time(DEFAULT_CLUSTER_TIME_MS)
    }
}

impl WebmMuxer {
    /// Create a muxer closing clusters after `cluster_time_ms` of audio.
    /// ([`Default`] uses [`DEFAULT_CLUSTER_TIME_MS`].)
    pub fn with_cluster_time(cluster_time_ms: u64) -> Self {
        Self {
            cluster: Vec::new(),
            cluster_open: false,
            cluster_start_ms: 0,
            max_cluster_time_ms: cluster_time_ms,
        }
    }

    /// The init segment: EBML header + open (unknown-size) `Segment` +
    /// `Info` + `Tracks`. Stateless; every generation emits identical
    /// bytes.
    pub fn init_segment() -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&Self::ebml_header());
        let mut segment_header = id_bytes(SEGMENT_ID);
        segment_header.extend_from_slice(&unknown_size());
        out.extend_from_slice(&segment_header);
        
        out.extend_from_slice(&element(INFO_ID, &Self::info_payload()));
        out.extend_from_slice(&element(TRACKS_ID, &Self::tracks_payload()));
        out
    }

    /// Append an Opus `packet` at absolute `timecode_ms` and return any
    /// clusters closed by this block (normally empty, or one).
    pub fn add_block(&mut self, timecode_ms: u64, packet: &[u8]) -> Vec<Vec<u8>> {
        // SimpleBlock: id (1) + size vint + track number (vint) + relative
        // timecode (2, big-endian sint16) + flags (1) + payload. Like every
        // other EBML element, a SimpleBlock carries a size vint (ffmpeg's
        // matroska muxer emits `A3 41 0d 81 ...` for a 13-byte block on
        // track 1; ffmpeg's parser requires both the size vint and the vint
        // track number).
        const { assert!(TRACK_NUMBER <= 0x7F, "1-byte track number vint"); }
        // Data after the size vint: track number (1) + relative timecode
        // (2) + flags (1) + payload.
        let data_len = 4 + packet.len();
        let size = vint_size(data_len as u64);
        let block_len = 1 + size.len() + data_len;
        let mut closed = Vec::new();
        let rel = timecode_ms.saturating_sub(self.cluster_start_ms);
        let over_time = self.cluster_open && rel >= self.max_cluster_time_ms;
        let over_bytes = self.cluster_open && self.cluster.len() + block_len > MAX_CLUSTER_BYTES;
        if over_time || over_bytes {
            closed.push(self.close_cluster());
        }
        if !self.cluster_open {
            self.cluster_start_ms = timecode_ms;
            self.cluster.clear();
            self.cluster_open = true;
        }
        let rel = timecode_ms - self.cluster_start_ms;
        let block = &mut self.cluster;
        block.push(SIMPLE_BLOCK_ID as u8);
        block.extend_from_slice(&size);
        block.push(0x80 | TRACK_NUMBER); // 1-byte vint
        block.extend_from_slice(&(rel as i16).to_be_bytes());
        block.push(FLAG_KEYFRAME);
        block.extend_from_slice(packet);
        closed
    }

    /// Force-close the open cluster (end of stream). `None` when no cluster
    /// is open.
    pub fn flush(&mut self) -> Option<Vec<u8>> {
        if self.cluster_open {
            Some(self.close_cluster())
        } else {
            None
        }
    }

    /// Close the open cluster into a complete `Cluster` element. The
    /// mandatory `Timecode` (absolute ms of the first block) precedes the
    /// blocks per the Matroska spec.
    fn close_cluster(&mut self) -> Vec<u8> {
        self.cluster_open = false;
        let mut payload = uint_element(TIMECODE_ID, self.cluster_start_ms);
        payload.extend(std::mem::take(&mut self.cluster));
        element(CLUSTER_ID, &payload)
    }

    /// The WebM-spec EBML header: EBML version/read version 1 (ffmpeg 8
    /// rejects read version > 1) and `DocTypeVersion` 2 for doctype
    /// "webm" (Matroska R4 files use 4; 2 is what the WebM profile asks
    /// for and what older demuxers expect).
    fn ebml_header() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&uint_element(0x4286, 1)); // Version
        p.extend_from_slice(&uint_element(0x42F7, 1)); // ReadVersion
        p.extend_from_slice(&string_element(0x4282, "webm"));
        p.extend_from_slice(&uint_element(0x4287, 2)); // DocTypeVersion
        element(EBML_HEADER_ID, &p)
    }

    /// `Info` payload: `TimestampScale` must be exactly 1 000 000 (timecodes
    /// in whole milliseconds) per the Chromium MSE requirements.
    fn info_payload() -> Vec<u8> {
        let mut p = Vec::new();
        p.extend_from_slice(&uint_element(TIMESTAMP_SCALE_ID, 1_000_000));
        p.extend_from_slice(&string_element(MUXING_APP_ID, "ubc125"));
        p.extend_from_slice(&string_element(WRITING_APP_ID, "ubc125"));
        p
    }

    /// `Tracks` payload: one enabled, default audio track, codec A_OPUS.
    fn tracks_payload() -> Vec<u8> {
        let audio = {
            let mut a = Vec::new();
            a.extend_from_slice(&float_element(SAMPLE_FREQ_ID, OPUS_SAMPLE_RATE as f64));
            a.extend_from_slice(&uint_element(CHANNELS_ID, 1));
            a
        };
        let mut entry = Vec::new();
        entry.extend_from_slice(&uint_element(TRACK_NUMBER_ID, TRACK_NUMBER as u64));
        entry.extend_from_slice(&uint_element(TRACK_TYPE_ID, 2)); // audio
        entry.extend_from_slice(&uint_element(FLAG_ENABLED_ID, 1));
        entry.extend_from_slice(&uint_element(FLAG_DEFAULT_ID, 1));
        entry.extend_from_slice(&string_element(NAME_ID, "audio"));
        entry.extend_from_slice(&string_element(CODEC_ID_ID, "A_OPUS"));
        entry.extend_from_slice(&binary_element(CODEC_PRIVATE_ID, &Self::opus_head()));
        entry.extend_from_slice(&element(AUDIO_ID, &audio));
        element(TRACK_ENTRY_ID, &entry)
    }

    /// The 19-byte `OpusHead` (RFC 7845 §5.1, little-endian). Pre-skip 312
    /// is the conventional libopus value: it hides encoder warm-up at the
    /// start of the stream.
    fn opus_head() -> [u8; 19] {
        let mut h = [0u8; 19];
        h[0..8].copy_from_slice(b"OpusHead");
        h[8] = 1; // version
        h[9] = 1; // channel count
        h[10..12].copy_from_slice(&312u16.to_le_bytes()); // pre-skip
        h[12..16].copy_from_slice(&OPUS_SAMPLE_RATE.to_le_bytes());
        // output_gain 0, mapping_family 0 already zero
        h
    }
}

/// Element id as its raw bytes (no leading zero padding).
///
/// EBML element ids are never zero, so the first non-zero byte always
/// exists; the debug assert turns a future zero-id bug into a clear
/// message instead of an opaque panic.
pub(crate) fn id_bytes(id: u32) -> Vec<u8> {
    debug_assert!(id != 0, "EBML element id must be non-zero");
    let be = id.to_be_bytes();
    let first = be.iter().position(|&b| b != 0).unwrap();
    be[first..].to_vec()
}

/// Canonical vint size encoding for `value` (same rules as the segmenter's
/// test fixtures): 1/2/4/8-byte widths; an all-ones encoding means unknown
/// size, so each width's top value steps up a width.
fn vint_size(value: u64) -> Vec<u8> {
    debug_assert!(
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
            (value >> 16) as u8,
            (value >> 8) as u8,
            value as u8,
        ]
    } else {
        let b = value.to_be_bytes();
        vec![0x01, b[1], b[2], b[3], b[4], b[5], b[6], b[7]]
    }
}

fn unknown_size() -> [u8; 8] {
    [0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
}

/// A complete EBML element: id + size vint + `payload`.
fn element(id: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = id_bytes(id);
    out.extend_from_slice(&vint_size(payload.len() as u64));
    out.extend_from_slice(payload);
    out
}

/// A uint element with the minimal **big-endian** byte width. EBML
/// unsigned integers are stored most-significant-byte first (the same
/// order as the size vints); ffmpeg's matroska demuxer reads
/// `0F 42 40` as 1 000 000 and would read `40 42 0F` as 4 211 215.
fn uint_element(id: u32, value: u64) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut v = value;
    while v > 0 {
        bytes.push((v & 0xFF) as u8);
        v >>= 8;
    }
    bytes.reverse();
    if bytes.is_empty() {
        bytes.push(0);
    }
    element(id, &bytes)
}

fn string_element(id: u32, value: &str) -> Vec<u8> {
    element(id, value.as_bytes())
}

/// A float element: 8-byte IEEE 754 **big-endian**, the same encoding
/// ffmpeg's muxer uses for `SamplingFrequency`.
fn float_element(id: u32, value: f64) -> Vec<u8> {
    element(id, &value.to_be_bytes())
}

fn binary_element(id: u32, value: &[u8]) -> Vec<u8> {
    element(id, value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::webm::{DEFAULT_MAX_SEGMENT_SIZE, Segment, WebmSegmenter};

    /// Deterministic pseudo-packets (the muxer does not inspect payloads):
    /// 40 bytes of `(frame * 7 + i) % 251`, 20 ms apart in time. Returns the
    /// full stream (init + clusters) and the closed clusters separately.
    fn feed_frames(muxer: &mut WebmMuxer, n_frames: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
        let mut out = WebmMuxer::init_segment();
        let mut clusters = Vec::new();
        for frame in 0..n_frames {
            let packet: Vec<u8> = (0..40).map(|i| ((frame * 7 + i) % 251) as u8).collect();
            let tc = frame as u64 * OPUS_FRAME_MS;
            for cluster in muxer.add_block(tc, &packet) {
                out.extend_from_slice(&cluster);
                clusters.push(cluster);
            }
        }
        if let Some(c) = muxer.flush() {
            out.extend_from_slice(&c);
            clusters.push(c);
        }
        (out, clusters)
    }

    /// G1: muxer output fed to the existing segmenter (one-shot) yields
    /// exactly one `Init` then N `Media`, byte-identical, for 1 s, 60 s, and
    /// a forced-flush-at-end shape. At the 60 ms default a cluster holds
    /// 3 frames, so 50 frames close 17 clusters (16 full + a flushed
    /// 2-frame tail), 3000 frames close 1000, and 7 frames close 3.
    #[test]
    fn round_trip_through_segmenter() {
        for (n_frames, expected_clusters) in [(50usize, 17usize), (3000, 1000), (7, 3)] {
            let (stream, clusters) = {
                let mut muxer = WebmMuxer::default();
                let (s, c) = feed_frames(&mut muxer, n_frames);
                (s, c)
            };
            assert_eq!(
                clusters.len(),
                expected_clusters,
                "cluster count for {n_frames} frames (60 ms / 64 KiB flush rules)"
            );
            let segments = WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE)
                .feed(&stream)
                .expect("segmenter must accept muxer output");
            assert_eq!(
                segments.len(),
                expected_clusters + 1,
                "init + {expected_clusters} clusters for {n_frames} frames"
            );
            let Segment::Init(init) = &segments[0] else {
                panic!("first segment must be Init");
            };
            // Byte identity: init + emitted clusters == the raw stream.
            let mut rebuilt = init.clone();
            for (i, segment) in segments[1..].iter().enumerate() {
                let Segment::Media(bytes) = segment else {
                    panic!("segment {i} after init must be Media");
                };
                assert_eq!(bytes, &clusters[i], "cluster {i} bytes differ");
                rebuilt.extend_from_slice(bytes);
            }
            assert_eq!(rebuilt, stream, "segments must reconstruct the stream");
        }
    }

    /// G1: one-byte-at-a-time feeding also round-trips.
    #[test]
    fn round_trip_one_byte_at_a_time() {
        let mut muxer = WebmMuxer::default();
        let (stream, clusters) = feed_frames(&mut muxer, 123);
        let mut segmenter = WebmSegmenter::new(DEFAULT_MAX_SEGMENT_SIZE);
        let mut segments = Vec::new();
        for b in &stream {
            segments
                .extend(segmenter.feed(std::slice::from_ref(b)).expect("feed"));
        }
        assert_eq!(segments.len(), clusters.len() + 1);
        let Segment::Init(init) = &segments[0] else {
            panic!("first segment must be Init")
        };
        let mut rebuilt = init.clone();
        for (i, segment) in segments[1..].iter().enumerate() {
            let Segment::Media(bytes) = segment else {
                panic!("media expected");
            };
            rebuilt.extend_from_slice(bytes);
            assert_eq!(bytes, &clusters[i]);
        }
        assert_eq!(rebuilt, stream);
    }

    // G2: field-level checks on the emitted init segment and clusters.
    //
    // A tiny recursive EBML walker (id, size, payload) parses our own
    // output; the assertions pin the exact values Chromium requires.

    /// Width of a vint from its first byte: 0x80-0xFF is 1 byte,
    /// 0x40-0x7F is 2, 0x10-0x3F is 4, 0x01-0x07 is 8.
    fn vint_width(first: u8) -> usize {
        match first {
            0x80..=0xFF => 1,
            0x40..=0x7F => 2,
            0x10..=0x3F => 4,
            0x01..=0x07 => 8,
            _ => panic!("invalid vint first byte {first:#04x}"),
        }
    }

    /// Width of an EBML element id. Element ids are fixed-width, not
    /// vints, so the width is disambiguated against the set of ids this
    /// muxer emits (the walker only parses our own output).
    fn id_width(rest: &[u8]) -> usize {
        const ONE: [u8; 19] = [
            0xA3, 0x4F, 0x50, 0x51, 0x56, 0x68, 0x75, 0x83, 0x86, 0x88, 0x9C,
            0x9F, 0xB5, 0xB6, 0xB9, 0xD7, 0xE1, 0xE7, 0xAE,
        ];
        const TWO: [u16; 8] = [
            0x4282, 0x4286, 0x4287, 0x42F7, 0x4D80, 0x536E, 0x5741, 0x63A2,
        ];
        const THREE: [u32; 2] = [0x002A_D7B1, 0x0040_420F];
        const FOUR: [u32; 5] = [
            0x1A45_DFA3, 0x1549_A966, 0x1654_AE6B, 0x1853_8067, 0x1F43_B675,
        ];
        if !rest.is_empty() && ONE.contains(&rest[0]) {
            return 1;
        }
        if rest.len() >= 2 {
            let id2 = u16::from_be_bytes([rest[0], rest[1]]);
            if TWO.contains(&id2) {
                return 2;
            }
        }
        if rest.len() >= 3 {
            let id3 = u32::from_be_bytes([rest[0], rest[1], rest[2], 0]) >> 8;
            if THREE.contains(&id3) {
                return 3;
            }
        }
        if rest.len() >= 4
            && FOUR
                .contains(&u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]))
        {
            return 4;
        }
        panic!(
            "unknown element id {:02x?} in test walker",
            &rest[..4.min(rest.len())]
        )
    }

    fn walk(bytes: &[u8]) -> Vec<(u32, Vec<u8>)> {
        let mut out = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let len = id_width(rest);
            let mut id = 0u32;
            for &b in &rest[..len] {
                id = (id << 8) | b as u32;
            }
            rest = &rest[len..];
            let slen = vint_width(rest[0]);
            let size_bytes = &rest[..slen];
            let data_bits = 7 * slen;
            let mask = if data_bits == 64 {
                u64::MAX
            } else {
                (1u64 << data_bits) - 1
            };
            let mut size = 0u64;
            for &b in size_bytes {
                size = (size << 8) | b as u64;
            }
            let size = size & mask;
            rest = &rest[slen..];
            // An all-ones size is "unknown": the element runs to the end
            // of the buffer (true for the top-level Segment), consuming
            // everything left.
            let payload = if size == mask {
                let payload = rest.to_vec();
                rest = &[];
                payload
            } else {
                let payload = rest[..size as usize].to_vec();
                rest = &rest[size as usize..];
                payload
            };
            out.push((id, payload));
        }
        out
    }

    /// Parse a Cluster payload: the Timecode element, then the
    /// `SimpleBlock` payloads (the generic walker handles the blocks' size
    /// vints).
    fn parse_cluster_payload(payload: &[u8]) -> (u64, Vec<Vec<u8>>) {
        let els = walk(payload);
        assert_eq!(els[0].0, TIMECODE_ID, "Timecode must be first");
        // EBML uints are big-endian: right-align the payload bytes.
        let mut tc_bytes = [0u8; 8];
        let len = els[0].1.len().min(8);
        tc_bytes[8 - len..].copy_from_slice(&els[0].1[..len]);
        let tc = u64::from_be_bytes(tc_bytes);
        let blocks = els[1..].iter().map(|(_, p)| p.clone()).collect();
        (tc, blocks)
    }

    fn find(els: &[(u32, Vec<u8>)], id: u32) -> &[u8] {
        els.iter()
            .find(|(eid, _)| *eid == id)
            .map(|(_, p)| p.as_slice())
            .unwrap_or_else(|| panic!("element 0x{id:08X} not found"))
    }

    #[test]
    fn init_segment_fields() {
        let init = WebmMuxer::init_segment();
        let top = walk(&init);
        assert_eq!(top[0].0, EBML_HEADER_ID);
        assert_eq!(top[1].0, SEGMENT_ID);
        // WebM-spec EBML header: version/read version 1 (ffmpeg 8 rejects
        // read version > 1), doctype "webm", DocTypeVersion 2.
        let ebml_els = walk(&top[0].1);
        assert_eq!(find(&ebml_els, 0x4286), &[1], "EBML version 1");
        assert_eq!(find(&ebml_els, 0x42F7), &[1], "EBML read version 1");
        assert_eq!(find(&ebml_els, 0x4282), b"webm");
        assert_eq!(find(&ebml_els, 0x4287), &[2], "DocTypeVersion 2");
        // The Segment element (after the 24-byte EBML header element) has
        // unknown size: all-ones vint.
        assert_eq!(&init[24..28], &id_bytes(SEGMENT_ID), "Segment id at 24");
        assert_eq!(
            &init[28..36],
            &[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF],
            "Segment must be unknown size"
        );
        // Only the EBML header and the Segment are top-level.
        assert_eq!(top.len(), 2);
        let seg_els = walk(&top[1].1);
        let info = find(&seg_els, INFO_ID).to_vec();
        let tracks = find(&seg_els, TRACKS_ID).to_vec();
        let info_els = walk(&info);
        let tracks_els = walk(&tracks);
        // Order: Info before Tracks, nothing else in the Segment header.
        assert_eq!(seg_els[0].0, INFO_ID, "Info must precede Tracks");
        assert_eq!(seg_els[1].0, TRACKS_ID);
        assert_eq!(seg_els.len(), 2);
        assert_eq!(
            find(&info_els, TIMESTAMP_SCALE_ID),
            &[0x0F, 0x42, 0x40],
            "TimestampScale must be exactly 1000000 (minimal 3-byte BE)"
        );
        let entry = find(&tracks_els, TRACK_ENTRY_ID).to_vec();
        let entry_els = walk(&entry);
        assert_eq!(find(&entry_els, TRACK_NUMBER_ID), &[1]);
        assert_eq!(find(&entry_els, TRACK_TYPE_ID), &[2]);
        assert_eq!(find(&entry_els, FLAG_ENABLED_ID), &[1]);
        assert_eq!(find(&entry_els, FLAG_DEFAULT_ID), &[1]);
        assert_eq!(find(&entry_els, CODEC_ID_ID), b"A_OPUS");
        // CodecPrivate: the exact 19-byte OpusHead from the plan.
        let expected_head = [
            b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', 1, 1, 0x38, 0x01, 0x80, 0xBB, 0x00,
            0x00, 0x00, 0x00, 0x00,
        ];
        assert_eq!(find(&entry_els, CODEC_PRIVATE_ID), &expected_head);
        let audio = find(&entry_els, AUDIO_ID).to_vec();
        let audio_els = walk(&audio);
        assert_eq!(
            f64::from_be_bytes(find(&audio_els, SAMPLE_FREQ_ID).try_into().unwrap()),
            48_000.0,
            "SamplingFrequency must be 48000 (8-byte BE, like ffmpeg)"
        );
        assert_eq!(find(&audio_els, CHANNELS_ID), &[1]);
    }

    #[test]
    fn cluster_and_block_layout() {
        let mut muxer = WebmMuxer::default();
        let _init = WebmMuxer::init_segment();
        let mut clusters = Vec::new();
        // 60 ms of frames is 3 frames: the time cap closes at frames 3, 6,
        // 9, …, and the final 2 of the 23 frames flush at the end.
        for frame in 0..23 {
            let packet = [0xAB; 10];
            let tc = frame as u64 * OPUS_FRAME_MS;
            clusters.extend(muxer.add_block(tc, &packet));
        }
        clusters.push(muxer.flush().unwrap());
        assert_eq!(clusters.len(), 8, "clusters at 0, 60, 120, …, 420 ms");
        let expected_starts: Vec<u64> = (0..8).map(|i| i * 60).collect();
        for (i, cluster) in clusters.iter().enumerate() {
            let els = walk(cluster);
            assert_eq!(els.len(), 1, "a cluster element has one id");
            assert_eq!(els[0].0, CLUSTER_ID);
            let (tc, blocks) = parse_cluster_payload(&els[0].1);
            assert_eq!(tc, expected_starts[i], "cluster {i} timecode");
            // B1: every time-closed cluster holds exactly 3 frames; only
            // the flushed tail is short.
            let expected_blocks = if i < 7 { 3 } else { 2 };
            assert_eq!(blocks.len(), expected_blocks, "blocks in cluster {i}");
            for (j, block) in blocks.iter().enumerate() {
                assert_eq!(block[0], 0x81, "track number (1-byte vint 1)");
                let rel = i16::from_be_bytes([block[1], block[2]]);
                assert_eq!(rel, j as i16 * 20, "relative timecode in cluster {i}");
                assert_eq!(block[3], FLAG_KEYFRAME, "keyframe flag on every block");
                assert_eq!(&block[4..], &[0xAB; 10], "payload preserved");
            }
        }
    }

    #[test]
    fn cluster_time_is_configurable() {
        // 100 ms: 5 frames per cluster; 50 frames fill exactly 10 clusters,
        // the last of which the time cap never closes (it would need a 51st
        // frame), so the final flush closes it.
        let mut muxer = WebmMuxer::with_cluster_time(100);
        let mut clusters = Vec::new();
        for frame in 0..50u64 {
            let packet = [0xAB; 10];
            clusters.extend(muxer.add_block(frame * OPUS_FRAME_MS, &packet));
        }
        assert_eq!(clusters.len(), 9, "time cap closed the first 9");
        clusters.push(muxer.flush().unwrap());
        assert_eq!(clusters.len(), 10, "flush closes the open tenth");
        for (i, cluster) in clusters.iter().enumerate() {
            let (tc, blocks) = parse_cluster_payload(&walk(cluster)[0].1);
            assert_eq!(tc, i as u64 * 100, "cluster {i} timecode");
            assert_eq!(blocks.len(), 5, "blocks in cluster {i}");
        }
    }

    #[test]
    fn byte_cap_closes_cluster() {
        let mut muxer = WebmMuxer::default();
        // 4 KiB packets with 1 ms-apart timecodes: the 60 ms time cap
        // never fires (only 16 ms of timecodes), so the 64 KiB byte cap
        // decides — it holds 15 blocks (a 16th would overflow).
        let big = [0u8; 4096];
        // 1-byte id + 2-byte size vint (data is 4100 > 126) + 4-byte header + payload.
        let block_len = 1 + 2 + 4 + big.len();
        let mut clusters = Vec::new();
        for frame in 0..16u64 {
            clusters.extend(muxer.add_block(frame, &big));
        }
        let last = muxer.flush().unwrap();
        clusters.push(last);
        // 15 blocks fill the cap; the 16th opens a second cluster, closed
        // by the flush.
        assert_eq!(clusters.len(), 2, "one byte-capped cluster + flushed tail");
        let count_blocks = |c: &Vec<u8>| {
            parse_cluster_payload(&walk(c)[0].1).1.len()
        };
        assert_eq!(count_blocks(&clusters[0]), 15);
        assert_eq!(count_blocks(&clusters[1]), 1);
        // The closed size is timecode element (3 B) + n blocks.
        assert_eq!(clusters[0].len(), 4 + 4 + 3 + 15 * block_len);
    }
}
