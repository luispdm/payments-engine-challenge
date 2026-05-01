//! Concurrency variants benchmarked under task 07b.
//!
//! Each submodule wraps the production [`crate::engine::Engine`] in a
//! different shared-state strategy and exposes the same observable
//! contract: a streaming `submit` entry point used by tests, and a
//! multi-producer `run_workload` entry point used by the criterion and
//! one-shot bench drivers.
//!
//! No `trait Engine` extraction: per `~/payments-engine-challenge-docs/decisions.md`
//! the variants' shapes diverge enough that a unified trait would force
//! awkward wrappers (e.g. the actor variants are sender handles, not
//! engines). Concrete types only; small driver duplication is accepted.
//!
//! The whole module is gated behind the `bench` Cargo feature so the
//! production build pulls in no `dashmap`, `crossbeam`, or
//! `hdrhistogram` dependency.

pub mod actor_crossbeam;
pub mod actor_std;
pub mod baseline;
pub mod dashmap_engine;
pub mod mutex;
pub mod workload;
