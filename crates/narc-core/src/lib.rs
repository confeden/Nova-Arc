//! narc-core — the NARC archive format (Nova Arc).
//!
//! Design goals of the format (v0):
//! - Cheap edits: the archive is an append-only log of content-defined chunks.
//!   Adding, replacing or removing files appends new chunks and a new manifest
//!   instead of rewriting the whole archive. Dead space is reclaimed by an
//!   explicit offline `compact` step.
//! - Two-phase compression: files are analyzed first (magic bytes + trial
//!   compression) to pick a storage method, then compressed chunk by chunk.
//! - Bounded memory: no operation ever needs more than a few chunk buffers
//!   (max chunk = 4 MiB), regardless of archive size.

#![forbid(unsafe_code)]

pub mod analyze;
pub mod archive;
pub mod codec;
pub mod footer;
pub mod manifest;
pub mod paths;
pub mod pipeline;

pub use analyze::Tier;
pub use archive::{AddStats, Archive, ExtractStats, InfoStats, Overwrite};
pub use codec::Codec;
pub use pipeline::PackOptions;
