//! Native Harness² support for the codex-local fork.
//!
//! The method has two recursively connected levels:
//!
//! * task-level recursion: reference rollout → INV/DOM proposals → probes → composition;
//! * harnessing-level recursion: reflection into a per-domain library of adopted harnesses.
//!
//! All model work is delegated to `codex exec --json`, so the method runs on the same
//! provider, model, reasoning effort, and sandbox policy as the interactive harness.

pub mod library;
pub mod prompts;
pub mod recursion;
pub mod runner;
pub mod surface;

pub use recursion::Mode;
pub use recursion::RunOptions;
pub use recursion::RunReport;
pub use recursion::run;
