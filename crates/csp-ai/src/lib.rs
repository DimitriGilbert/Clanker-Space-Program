//! Classical automation: guidance, design solver, transfer optimizer (PLAN §2.11).
//! LlmAdapter trait — LLM emits SimCommands through the same surface.
//!
//! # Consumers of the transfer planner
//!
//! The planner itself lives in `csp_sim::transfer`
//! (`lambert`/`porkchop_scan`/`plan_transfer`/`predict_encounter`/
//! `departure_node`) with `SimCommand::WatchTransferWindow` as its
//! in-sim surfacing surface. It is deliberately consumer-free at M5:
//! nothing yet composes `plan_transfer → departure_node →
//! predict_encounter` into a flown mission — that loop is the M9a
//! automation milestone's job (the orchestrator that re-plans around
//! the documented `departure_node` mid-course correction), tracked in
//! PLAN M9a. The M5 in-sim proof that the underlying world machinery
//! completes such a transfer lives in csp-sim
//! (`duva_reachable_in_sim_end_to_end`).
