//! Live scanner audio: capture sources, WebM segmentation, and broadcast
//! to gRPC subscribers.

pub mod alsacapture;
pub mod broadcaster;
pub mod native;
pub mod opusenc;
pub mod source;
pub mod webm;
pub mod webm_mux;

pub use broadcaster::{AudioBroadcaster, AudioError, AudioEvent, AudioSubscription};
pub use native::{AlsaOpusSource, ToneSource};
pub use source::{CaptureSource, CommandSource, DEFAULT_AUDIO_DEVICE};
