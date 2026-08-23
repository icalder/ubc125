# Reference audio captures

Gitignored 48 kHz mono 16-bit captures of the UBC125XLT scanner's audio
input, used as the ground-truth target for tuning the de-clicker. They
are not committed (large binary); regenerate as below.

## Capturing: raw ALSA PCM (what the de-clicker actually sees)

The de-clicker runs on the Pi **between the ALSA PCM and the Opus
encoder** (`serve --declick`), so the tuning target is the raw PCM — not
the Opus-decoded `Listen` stream. The old `unfiltered.wav`
(`audio_dump` round-trip) was validated against the wrong signal: Opus
is lossy and shifts the noise floor and low-level content where the
click/voice discrimination lives. It has been deleted.

Capture on the Pi, stream to the dev machine over an SSH pipe. Use
`arecord` on the same device and format as the production open
(`hw:2`, 48 kHz, mono, S16_LE — the `AlsaReader` open in
[src/audio/alsacapture.rs](../src/audio/alsacapture.rs)):

```sh
# from the dev machine; the Pi is reachable as `alarmpi`
ssh alarmpi 'arecord -D hw:2 -f S16_LE -r 48000 -c 1 -t raw -d 60' > /tmp/raw.s16
```

While it runs, put the scanner in scan mode so the capture contains
channel-switch clicks (in voice-free gaps) plus the speech to preserve.
`ubc125 serve` may keep running; just have no active `Listen` client
(only an active stream holds the device). `-d N` is approximate —
verify length afterwards: `bytes / 2 / 48000` seconds.

Then wrap the raw samples in a WAV header and drop the file here:

```sh
python3 - <<'EOF'
import struct
raw = open('/tmp/raw.s16', 'rb').read()
open('test-audio/raw60.wav', 'wb').write(
    b'RIFF' + struct.pack('<I', 36 + len(raw)) + b'WAVE'
    + b'fmt ' + struct.pack('<IHHIIHH', 16, 1, 1, 48000, 96000, 2, 16)
    + b'data' + struct.pack('<I', len(raw)) + raw)
EOF
```

## Current captures

| file         | contents                                                                                                      |
|--------------|---------------------------------------------------------------------------------------------------------------|
| `raw60.wav`  | 60 s raw ALSA PCM (2026-08-23): scanning with channel-switch clicks and speech; floor ≈ −57…−73 dBFS         |

The click catalogue and speech-region annotations from the old 20 s
capture are gone with it; rebuild them for `raw60.wav` before the next
A/B round (the A/B harness is
[examples/wavelet_declick.rs](../examples/wavelet_declick.rs), with
`rt`/`offline` modes and `--soft`/`--k`/`--tail`/`--contrast`/`--ref`).
