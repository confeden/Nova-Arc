//! narc-core — the NARC archive format (Nova Arc).
//!
//! Design goals of the format (v0):
//! - Cheap edits: the archive is an append-only log of content-defined chunks.
//!   Adding, replacing or removing files appends new chunks and a new manifest
//!   instead of rewriting the whole archive. Dead space is reclaimed by an
//!   explicit offline `compact` step.
//! - Two-phase compression: files are analyzed first (format magic, content
//!   class, trial compression) to pick a codec and a reversible filter, then
//!   compressed chunk by chunk. Small files are packed into solid blocks so
//!   the compressor can exploit redundancy between them.
//! - Bounded memory: no operation ever needs more than a few chunk buffers
//!   (max chunk = 4 MiB), regardless of archive size.

#![forbid(unsafe_code)]

pub mod analyze;
pub mod archive;
pub mod codec;
pub mod deflate;
pub mod filters;
pub mod footer;
pub mod manifest;
mod pack;
pub mod paths;
pub mod pipeline;

pub use analyze::{Plan, Tier};
pub use archive::{
    AddStats, Archive, ExtractStats, InfoStats, Overwrite, Phase, Progress, UnitInfo,
};
pub use codec::Codec;
pub use filters::Filter;
pub use pipeline::PackOptions;
