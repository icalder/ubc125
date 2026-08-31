//! Live scanner audio: capture sources, WebM segmentation, and broadcast
//! to gRPC subscribers.

pub mod alsacapture;
pub mod broadcaster;
// The binary compiles this module tree too (main.rs declares `mod audio;`)
// but only wires `InPlaceDeClick`; the rest of the rig (the offline CLI, WAV
// I/O, the checks) is used through the library target by examples and tests.
#[allow(dead_code, unused_imports)]
pub mod clickfilter;
pub mod filter;
pub mod native;
pub mod opusenc;
pub mod source;
pub mod stats;
pub mod webm;
pub mod webm_mux;

pub use broadcaster::{
    AudioBroadcaster, AudioError, AudioEvent, AudioSubscription, DEFAULT_SUBSCRIBER_QUEUE,
};
pub use clickfilter::InPlaceDeClick;
pub use native::{AlsaOpusSource, ToneSource};
pub use source::{CaptureSource, CommandSource, DEFAULT_AUDIO_DEVICE};
