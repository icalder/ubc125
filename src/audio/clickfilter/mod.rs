//! The de-click prototype, ported to Rust: WAV in, corrected WAV out.
//!
//! This is the reference implementation's 1:1 port (`../ubc125-ml/scripts/clickfilter/`) and
//! the shape the parent project's production integration uses. Nothing here is a
//! model, a threshold, or an acceptance result: the corpus has no human labels
//! yet, so this is a listening and measurement rig whose development policy was
//! selected by the T3 round (`../ubc125-ml/docs/prototype.md`), not validated.
//!
//! Runtime rule 11 (`../ubc125-ml/AGENTS.md`): this path is direct arithmetic on i16 samples,
//! with no Burn, no CUDA and no ML runtime — the default feature set of the
//! crate has no dependencies at all.
//!
//! Modules, named as the Python package names them:
//!
//! | module       | owns                                                            |
//! |--------------|-----------------------------------------------------------------|
//! | `constants`  | physical constants, click classes, policies                     |
//! | `config`     | `Config`: every tunable, per-class policy, delay floor          |
//! | `ring`       | `PcmRing`: the one store the filter reads, decorates, emits from |
//! | `detect`     | `PlateauTrigger`: causal candidate detection, `classify`         |
//! | `fill`       | the correction targets (interp / descend / mute / lf-null), blends |
//! | `filter`     | `ClickFilter`, `GainPlan`, `run_filter`                          |
//! | `runtime`    | `InPlaceDeClick`: the production 960-sample seam (the parent's   |
//! |              | `PcmFrameFilter` trait)                                          |
//! | `checks`     | pass-through, residual, seam, per-class profile                  |
//! | `wav`        | WAV I/O                                                        |
//! | `format`     | Python-compatible number text, for the byte comparison           |
//! | `cli`        | argument parsing, the artifact tag, the run loop                 |

pub mod checks;
pub mod cli;
pub mod config;
pub mod constants;
pub mod detect;
pub mod fill;
pub mod filter;
pub mod format;
pub mod ring;
pub mod runtime;
pub mod wav;

pub use config::{Config, ConfigBuilder, ConfigError};
pub use constants::{CLASS_BOUNDS, ClickClass, FRAME, FS, Polarity, Policy, RATE, ms_to_samples};
pub use detect::{Candidate, PlateauTrigger, classify};
pub use filter::{ClickFilter, Decision, EventRecord, GainPlan, Metrics, run_filter};
pub use ring::PcmRing;
pub use runtime::InPlaceDeClick;

/// Every class the vocabulary knows, in plateau-length order — the rig's
/// `CLASSES`.
pub const CLASSES: [ClickClass; 4] = ClickClass::ALL;
