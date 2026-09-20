//! Desktop telemetry subsystem (opt-in, anonymous).
//!
//! The reusable plumbing now lives in the `splitlane-telemetry`
//! workspace crate. This module re-exports `client` and provides thin
//! domain shims for `id` (path resolution wrapper) and `tags` (the
//! `InstallMethod`/`UpdateError` mapping that depends on `crate::update`
//! types and therefore stays inside the desktop binary).
//!
//! Submodules:
//! - `client` - re-exported from `splitlane-telemetry::client`.
//! - `id` - desktop shim that resolves `runtime_paths::data_dir()`
//!   then delegates to `splitlane_telemetry::id::telemetry_id_at`.
//! - `tags` - domain mapping for `InstallMethod` and `UpdateError`
//!   (kept here because those types live in `crate::update::*`); the
//!   canonical-tag format invariant moved to `splitlane_telemetry::tags`.
//!
//! The `SplitlaneApp`-bound `emit_*` wrappers live in `crate::app::telemetry_events`;
//! this module owns the plumbing (client re-export, id resolution, tag
//! flattening) while the capture-site helpers stay close to the rest of
//! the app surface.
//!
//! Invariants shared across the subsystem:
//! - No event is ever emitted unless `config.telemetry.enabled == Some(true)`
//!   **and** no kill-switch env var is set (`SPLITLANE_NO_TELEMETRY`,
//!   `DO_NOT_TRACK`, `NO_TELEMETRY`).
//! - No PII, no paths, no terminal content is ever transmitted - enforced
//!   at every call site.

pub use splitlane_telemetry::client;

pub mod id;
pub mod tags;
