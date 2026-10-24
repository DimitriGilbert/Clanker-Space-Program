//! The §3.2 experiment definitions — `data/experiments/<id>.ron`.
//!
//! An `Experiment` *module* on a part (§3.1 `ModuleDef::Experiment`)
//! says what the instrument can do (`data_units` per run, where it may
//! run); the [`ExperimentDef`] says what its results are worth —
//! the per-experiment `transmit_ratio` (§3.2's transmission-vs-recovery
//! split) and the diminishing-returns caps.
//!
//! `cap` is authored per `(body, situation)` per §3.2's
//! "`cap_key` is authored per `(experiment, body, situation)`" — one
//! [`ExperimentCap`] entry covers a body and a situation mask. The
//! soft-cap ledger itself keys on the full `(experiment, body, biome,
//! situation)` — biome is deliberately NOT in the authored cap: every
//! biome of a `(body, situation)` gets its own `collected` counter
//! against the same authored cap (§2.2's "science keys on
//! `(body, biome, situation)`").

use serde::{Deserialize, Serialize};

use crate::ids::{BodyId, ExperimentId, LocKey};
use crate::part::{Situation, SituationMask};

/// The `schema_version` this loader understands for
/// `data/experiments/*.ron` — its own namespace, like `balance.ron`
/// (§3.1 first-field convention).
pub const EXPERIMENT_SCHEMA_VERSION: u32 = 1;

/// One authored cap entry: the experiment's soft-cap `cap_key` (in data
/// units) for `body` in each of `situations`. Entries must not overlap
/// on a `(body, situation)` pair — the validator rejects ambiguity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentCap {
    /// The body this cap applies to.
    pub body: BodyId,
    /// Which situations the cap covers (non-empty, validator).
    pub situations: SituationMask,
    /// `cap_key` — the asymptotic cumulative-data ceiling per
    /// `(experiment, body, biome, situation)` key, in data units.
    pub cap_data_units: f64,
}

/// `data/experiments/<id>.ron` — one experiment kind (§3.2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExperimentDef {
    /// First field of every data file (§3.1). Must equal
    /// [`EXPERIMENT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Stable experiment ID (`exp.materials_study`) — also the file's
    /// stem, so the validator can check `id` ↔ filename agreement.
    /// `ModuleDef::Experiment::kind` references this.
    pub id: ExperimentId,
    /// Loc key for the display name.
    pub display_name: LocKey,
    /// §3.2's transmission ratio — the fraction of a record's data a
    /// remote uplink banks (authored band `0.2–1.0`; recovery yields
    /// 100% and closes the key).
    pub transmit_ratio: f64,
    /// The authored `(body, situation)` caps.
    pub caps: Vec<ExperimentCap>,
}

impl ExperimentDef {
    /// The authored `cap_key` for `(body, situation)` — `None` when no
    /// entry covers it (the experiment produces no bankable data in
    /// that context; the sim rejects the run).
    pub fn cap_for(&self, body: &BodyId, situation: Situation) -> Option<f64> {
        self.caps
            .iter()
            .find(|c| c.body == *body && c.situations.contains(situation))
            .map(|c| c.cap_data_units)
    }
}
