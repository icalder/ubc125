//! Diagnostic: dump live `AudioService/Listen` stream(s) to WebM + WAV and
//! check cluster timecode continuity.
//!
//! Usage:
//!   cargo run --example audio_dump [addr] [prefix] [seconds] [streams] [join-delay-secs]
//!
//!   addr             default http://192.168.1.90:50051
//!   prefix           default /tmp/ubc125-dump (writes {prefix}_s{i}.webm/.wav)
//!   seconds          capture duration, default 20
//!   streams          concurrent Listen streams (two-browser scenario), default 1
//!   join-delay-secs  stream 1+ start this many seconds after stream 0, default 3
//!   stopgap-secs     >0: stop->replay mode (streams must be 1): capture
//!                    {seconds}s, StopCapture, wait {stopgap}s, capture
//!                    {seconds}s again as a second generation
//!                    ({prefix}_s0a.* / {prefix}_s0b.*)
//!   play             "play": no files; stream the decoded audio as a
//!                    (size-unknown) WAV to stdout so the exact server
//!                    bytes can be heard by a non-browser client:
//!
//!      cargo run --example audio_dump http://192.168.1.90:50051 /tmp/x 30 1 0 0 play \
//!        | paplay
//!
//! Each stream is checked independently: every media chunk is one complete
//! WebM cluster whose absolute timecode must continue where the previous
//! cluster ended (200 ms steps). A repeated section shows up as an
//! overlap; a lost section as a gap; a replayed start as a late stream
//! whose first timecode is back at 0.
//!
//! The decoded WAVs are what the browser would play: `aplay {prefix}_s0.wav`
//! (48 kHz mono) or `ffplay {prefix}_s0.webm`.

use std::hash::{Hash, Hasher};
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A sink shared between capture phases without lifetime friction: all
/// writes are synchronous, so a std Mutex never spans an await.
type SharedSink = Arc<Mutex<Box<dyn Write + Send>>>;

use std::collections::HashSet;
use tokio_stream::StreamExt;
use ubc125_grpc::ubc125::v1::audio_service_client::AudioServiceClient;
use ubc125_grpc::ubc125::v1::SubscribeAudioRequest;

const OPUS_FRAME_MS: u64 = 20;
const OPUS_FRAME_SAMPLES: usize = 960; // 20 ms @ 48 kHz
const OPUS_PRE_SKIP: usize = 312; // OpusHead pre-skip, like the browser

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    rt.block_on(run());
}

async fn run() {
    let args: Vec<String> = std::env::args().collect();
    let addr = args.get(1).cloned().unwrap_or_else(|| "http://192.168.1.90:50051".into());
    let prefix = args.get(2).cloned().unwrap_or_else(|| "/tmp/ubc125-dump".into());
    let secs: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(20);
    let n_streams: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1).max(1);
    let join_delay: u64 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(3);
    let stopgap: u64 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(0);
    let play = args.get(7).map(|s| s == "play").unwrap_or(false);
    if stopgap > 0 && n_streams != 1 {
        eprintln!("stopgap mode only supports a single stream");
        std::process::exit(1);
    }

    let client = match AudioServiceClient::connect(addr.clone()).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("connect to {addr} failed: {e}");
            std::process::exit(1);
        }
    };

    let mut handles = Vec::new();
    if stopgap > 0 {
        let mut client = client.clone();
        let prefix_i = prefix.clone();
        handles.push(tokio::spawn(async move {
            // One sink across both generations: a single WAV stream with
            // real silence during the gap (keeps idle-pipe players alive
            // and mirrors the browser's stop->play experience).
            let sink: Option<SharedSink> = play.then(|| make_play_sink("s0a"));
            let a = dump_stream(
                client.clone(),
                prefix_i.clone(),
                "s0a".into(),
                secs,
                sink.clone(),
            )
            .await;
            eprintln!("stopgap: StopCapture after phase a; {stopgap}s of silence; then a new generation");
            client
                .stop_capture(ubc125_grpc::ubc125::v1::StopCaptureRequest::default())
                .await
                .expect("stop_capture");
            if let Some(s) = &sink {
                let mut g = s.lock().unwrap();
                let chunk = [0u8; 4096]; // 205 ms of silence
                let mut remaining = (stopgap * 48_000 * 2) as usize;
                while remaining > 0 {
                    let n = chunk.len().min(remaining);
                    g.write_all(&chunk[..n]).expect("write gap silence");
                    remaining -= n;
                }
                g.flush().expect("flush gap silence");
            }
            tokio::time::sleep(Duration::from_secs(stopgap)).await;
            let b = dump_stream(client, prefix_i, "s0b".into(), secs, sink.clone()).await;
            vec![a, b]
        }));
    } else {
        for i in 0..n_streams {
            let client = client.clone();
            let prefix_i = prefix.clone();
            handles.push(tokio::spawn(async move {
                if i > 0 {
                    tokio::time::sleep(Duration::from_secs(join_delay)).await;
                }
                let name = i.to_string();
                let sink: Option<SharedSink> = play.then(|| make_play_sink(&name));
                vec![dump_stream(client, prefix_i, name, secs, sink).await]
            }));
        }
    }
    let mut results = Vec::new();
    for h in handles {
        results.extend(h.await.expect("stream task"));
    }

    println!("\n== per-stream ==");
    for r in &results {
        println!("{r}");
    }
    if stopgap == 0 {
        println!("\n== cross-stream ==");
        for (i, r) in results.iter().enumerate() {
            if i == 0 {
                continue;
            }
            match r.first_tc {
                Some(tc) if tc > 1000 => println!(
                    "stream {i} joined LIVE at tc {tc} ms (expected ~{} ms after join delay)",
                    join_delay * 1000
                ),
                Some(tc) => println!(
                    "stream {i} first tc {tc} ms is NOT live — the generation's early seconds were replayed/resent"
                ),
                None => println!("stream {i} received no media"),
            }
        }
    }
    let bad = results
        .iter()
        .any(|r| r.dups > 0 || !r.overlaps.is_empty() || !r.gaps.is_empty());
    if !bad {
        println!("\nOK: every stream is a clean, gap-free, duplicate-free cluster sequence");
    } else if !play {
        println!("\nPROBLEMS FOUND: see per-stream report above");
        std::process::exit(1);
    }
}

/// Live playback sink: a streaming WAV (48 kHz mono s16le, unknown size —
/// the 0xFFFFFFFF convention) on stdout, to be piped into a local player
/// (`paplay`, `ffplay -i pipe:0 -`). Progress markers go to stderr, so the
/// pipe stays clean.
fn make_play_sink(name: &str) -> SharedSink {
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        eprintln!("stream {name}: WARNING: stdout is a terminal; pipe it into a player, e.g. | paplay");
    } else {
        eprintln!("stream {name}: live WAV (48 kHz mono s16le) on stdout");
    }
    let mut out = std::io::BufWriter::new(std::io::stdout());
    write_wav_header(&mut out, u32::MAX).expect("wav header");
    out.flush().expect("flush wav header");
    Arc::new(Mutex::new(Box::new(out)))
}

/// A 44-byte WAV header; `data_size` is 0xFFFFFFFF for streaming.
fn write_wav_header<W: Write>(w: &mut W, data_size: u32) -> std::io::Result<()> {
    // Streaming convention: unknown sizes stay 0xFFFFFFFF (no +36).
    let riff_size = if data_size == u32::MAX {
        u32::MAX
    } else {
        36 + data_size
    };
    w.write_all(b"RIFF")?;
    w.write_all(&riff_size.to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes());
    w.write_all(&1u16.to_le_bytes()); // PCM
    w.write_all(&1u16.to_le_bytes()); // mono
    w.write_all(&48_000u32.to_le_bytes());
    w.write_all(&(48_000u32 * 2).to_le_bytes()); // byte rate
    w.write_all(&2u16.to_le_bytes()); // block align
    w.write_all(&16u16.to_le_bytes()); // bits
    w.write_all(b"data")?;
    w.write_all(&data_size.to_le_bytes())
}

struct StreamResult {
    name: String,
    n_init: u32,
    n_media: u32,
    total_bytes: usize,
    dups: u32,
    overlaps: Vec<(i64, i64)>,
    gaps: Vec<(i64, i64)>,
    first_tc: Option<i64>,
    last_end_ms: Option<i64>,
    wav_bytes: usize,
}

impl std::fmt::Display for StreamResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "stream {}: {} init, {} media, {} bytes, wav {} bytes, tc {}..{}",
            self.name,
            self.n_init,
            self.n_media,
            self.total_bytes,
            self.wav_bytes,
            self.first_tc.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
            self.last_end_ms.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
        )?;
        if self.dups == 0 && self.overlaps.is_empty() && self.gaps.is_empty() {
            write!(f, "  OK")
        } else {
            write!(
                f,
                "  PROBLEMS: dups={} overlaps={:?} gaps={:?}",
                self.dups, self.overlaps, self.gaps
            )
        }
    }
}

async fn dump_stream(
    client: AudioServiceClient<tonic::transport::Channel>,
    prefix: String,
    name: String,
    secs: u64,
    sink: Option<SharedSink>,
) -> StreamResult {
    let play = sink.is_some();
    let webm_path = format!("{prefix}_{name}.webm");
    let wav_path = format!("{prefix}_{name}.wav");
    let mut client = client;
    let mut stream = match client.listen(SubscribeAudioRequest::default()).await {
        Ok(r) => r.into_inner(),
        Err(e) => {
            eprintln!("stream {name}: listen failed: {e}");
            return StreamResult {
                name: name.clone(),
                n_init: 0,
                n_media: 0,
                total_bytes: 0,
                dups: 0,
                overlaps: vec![],
                gaps: vec![],
                first_tc: None,
                last_end_ms: None,
                wav_bytes: 0,
            };
        }
    };

    let mut webm = (!play)
        .then(|| std::io::BufWriter::new(std::fs::File::create(&webm_path).expect("webm file")));
    let mut decoder =
        opus::Decoder::new(48_000, opus::Channels::Mono).expect("opus decoder");
    let mut pcm: Vec<i16> = Vec::new();
    let mut res = StreamResult {
        name: name.clone(),
        n_init: 0,
        n_media: 0,
        total_bytes: 0,
        dups: 0,
        overlaps: vec![],
        gaps: vec![],
        first_tc: None,
        last_end_ms: None,
        wav_bytes: 0,
    };
    let mut seen: HashSet<u64> = HashSet::new();
    let mut pre_skip = OPUS_PRE_SKIP;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);

    loop {
        let chunk = match tokio::time::timeout_at(deadline, stream.next()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(e))) => {
                eprintln!("stream {name}: stream error: {e}");
                break;
            }
            Ok(None) => {
                eprintln!("stream {name}: stream ended");
                break;
            }
            Err(_) => break, // deadline
        };
        res.total_bytes += chunk.payload.len();
        if let Some(w) = webm.as_mut() {
            w.write_all(&chunk.payload).expect("write webm");
        }
        if chunk.init_segment {
            res.n_init += 1;
            if res.n_init > 1 {
                eprintln!("stream {name}: !! second init segment at media #{}", res.n_media);
            }
            continue;
        }
        res.n_media += 1;
        let Some((tc, blocks)) = cluster_parts(&chunk.payload) else {
            eprintln!("stream {name}: !! media chunk {} is not a clean cluster", res.n_media);
            continue;
        };
        if res.first_tc.is_none() {
            res.first_tc = Some(tc);
        }
        let h = {
            let mut s = std::collections::hash_map::DefaultHasher::new();
            chunk.payload.hash(&mut s);
            s.finish()
        };
        if !seen.insert(h) {
            res.dups += 1;
            eprintln!("stream {name}: !! chunk {} (tc {tc}) is a byte-duplicate", res.n_media);
        }
        if let Some(prev_end) = res.last_end_ms {
            if tc < prev_end {
                res.overlaps.push((prev_end, tc));
                eprintln!("stream {name}: !! OVERLAP: chunk {} tc {tc} < previous end {prev_end}", res.n_media);
            } else if tc > prev_end {
                res.gaps.push((prev_end, tc));
                eprintln!("stream {name}: !! GAP: chunk {} tc {tc} > previous end {prev_end}", res.n_media);
            }
        }
        // Decode the Opus packets in this cluster (WAV, or live sink).
        for packet in block_packets(blocks) {
            let mut buf = [0i16; OPUS_FRAME_SAMPLES * 2];
            match decoder.decode(&packet, &mut buf, false) {
                Ok(n) => {
                    let n = n.min(buf.len());
                    let start = pre_skip.min(n);
                    pre_skip = pre_skip.saturating_sub(n);
                    let samples = &buf[start..n];
                    if let Some(s) = &sink {
                        let mut g = s.lock().unwrap();
                        let mut bytes = Vec::with_capacity(samples.len() * 2);
                        for smp in samples {
                            bytes.extend_from_slice(&smp.to_le_bytes());
                        }
                        g.write_all(&bytes).expect("play sink");
                    } else {
                        pcm.extend_from_slice(samples);
                    }
                }
                Err(e) => eprintln!("stream {name}: opus decode error: {e}"),
            }
        }
        if play {
            if let Some(s) = &sink {
                s.lock().unwrap().flush().expect("flush play sink");
            }
            if res.n_media % 25 == 0 {
                eprintln!("stream {name}: t={}s", res.n_media * 200 / 1000);
            }
        }
        let dur = cluster_duration_ms(&chunk.payload).unwrap_or(200);
        res.last_end_ms = Some(tc + dur as i64);
    }
    if let Some(w) = webm.as_mut() {
        w.flush().expect("flush webm");
    }
    if let Some(s) = &sink {
        s.lock().unwrap().flush().expect("flush play sink");
    }

    // WAV: 48 kHz mono s16le.
    let data_len = pcm.len() * 2;
    let mut wav = Vec::with_capacity(44 + data_len);
    let mut hdr: Vec<u8> = Vec::with_capacity(44);
    write_wav_header(&mut hdr, data_len as u32).expect("wav header");
    wav.extend_from_slice(&hdr);
    for s in &pcm {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    if play {
        eprintln!("stream {name}: playback done");
    } else {
        std::fs::write(&wav_path, &wav).expect("write wav");
        res.wav_bytes = wav.len();
        eprintln!("stream {name}: wrote {webm_path} and {wav_path}");
    }
    res
}

/// Split a cluster chunk into (timecode_ms, blocks).
/// Layout: `1F 43 B6 75` + size vint + `E7` + size vint + minimal-BE-uint
/// timecode, then the SimpleBlocks.
fn cluster_parts(bytes: &[u8]) -> Option<(i64, &[u8])> {
    let rest = bytes.strip_prefix(&[0x1F, 0x43, 0xB6, 0x75])?;
    let w = vint_width(rest[0])?.0;
    let payload = &rest[w..];
    if payload.first() != Some(&0xE7) {
        return None;
    }
    let tc_rest = &payload[1..];
    let w2 = vint_width(tc_rest[0])?.0;
    let data_len = vint_value(&tc_rest[..w2]) as usize;
    let tc_bytes = tc_rest.get(w2..w2 + data_len)?;
    if tc_bytes.len() > 8 {
        return None;
    }
    let mut v = 0i64;
    for &b in tc_bytes {
        v = (v << 8) | i64::from(b);
    }
    Some((v, &tc_rest[w2 + data_len..]))
}

/// The Opus packets in a cluster, in block order.
fn block_packets(blocks: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut p = blocks;
    while let Some((&b, tail)) = p.split_first() {
        if b != 0xA3 {
            break;
        }
        let w = match vint_width(tail[0]) {
            Some((w, _)) => w,
            None => break,
        };
        let data_len = vint_value(&tail[..w]) as usize;
        let data = match tail.get(w..w + data_len) {
            Some(d) if d.len() >= 4 => d,
            _ => break,
        };
        out.push(data[4..].to_vec());
        p = &tail[w + data_len..];
    }
    out
}

/// Cluster duration in ms: (last SimpleBlock relative timecode) + 20.
fn cluster_duration_ms(bytes: &[u8]) -> Option<u64> {
    let (_, blocks) = cluster_parts(bytes)?;
    let mut p = blocks;
    let mut last_rel: u64 = 0;
    while let Some((&b, tail)) = p.split_first() {
        if b != 0xA3 {
            break;
        }
        let w = vint_width(tail[0])?.0;
        let data_len = vint_value(&tail[..w]) as usize;
        let data = tail.get(w..w + data_len)?;
        if data.len() < 4 {
            return None;
        }
        last_rel = u16::from_be_bytes([data[1], data[2]]) as u64;
        p = &tail[w + data_len..];
    }
    Some(last_rel + OPUS_FRAME_MS)
}

/// Width of an EBML size vint from its first byte.
fn vint_width(first: u8) -> Option<(usize, u64)> {
    match first {
        0x80..=0xFF => Some((1, u64::from(first & 0x7F))),
        0x40..=0x7F => Some((2, 0)),
        0x10..=0x3F => Some((4, 0)),
        0x01..=0x07 => Some((8, 0)),
        _ => None,
    }
}

/// Decode a vint given its leading bytes.
fn vint_value(bytes: &[u8]) -> u64 {
    let w = vint_width(bytes[0]).expect("valid vint").0;
    let mut v = 0u64;
    for &b in &bytes[..w] {
        v = (v << 8) | u64::from(b);
    }
    let data_bits = 7 * w as u32;
    v & ((1u64 << data_bits) - 1)
}
