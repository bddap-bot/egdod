//! egdod: the first process on a bare machine dials out and hands a controller
//! three powers over it — exec, copy, forward. See `SPEC.md`.

pub mod agent;
pub mod controller;
pub mod net;
pub mod pipe;
pub mod proto;
pub mod ssh;
pub mod state;
