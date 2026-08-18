//! Live scanner audio: capture source, WebM segmentation, and broadcast to
//! gRPC subscribers.

pub mod broadcaster;
pub mod ffmpeg;
pub mod webm;

pub use broadcaster::{AudioBroadcaster, AudioError, AudioEvent, AudioSubscription};
pub use ffmpeg::{CaptureSource, CommandSource, DEFAULT_AUDIO_DEVICE, FfmpegSource};
