//! M9c §2.12.2 — the published telemetry snapshot.
//!
//! `SimStep` (or the headless host) builds an immutable
//! [`TelemetrySnapshot`] once per frame and publishes it into an
//! `ArcSwap`; readers (WS, HTTP, MCP) load the `Arc` and never block
//! the frame loop. The snapshot is a *pure function of world state* —
//! `csp-sim`'s `VesselTelemetry` plus the §2.2.1 control lease, never
//! hashed, never authoritative.
//!
//! # Field paths
//!
//! Widgets subscribe by *field path* — a dotted selector into the
//! snapshot's serialized shape (`"tick"`, `"warp_level"`,
//! `"vessels.0.altitude_m"`, `"vessels.0.control_lease"`). The wire
//! protocol sends only declared paths (§2.12.2: "the server sends
//! only those. There is no 'send me everything' request").

use std::collections::BTreeMap;

use csp_sim::{Lease, VesselTelemetry, World};
use serde::Serialize;

/// One per-frame immutable snapshot — the `ArcSwap` payload.
#[derive(Clone, Debug, Serialize)]
pub struct TelemetrySnapshot {
    /// `World::clock().ticks()` at build time.
    pub tick: u64,
    /// The stored warp ladder rung (§2.6.3).
    pub warp_level: u32,
    /// Vessel telemetry + control lease, keyed by `VesselId.0` —
    /// `BTreeMap` order is ascending id.
    pub vessels: BTreeMap<u64, SnapshotVessel>,
    /// The host's focused vessel — `None` when no focus is set (the
    /// sim has no focus concept; the host records the session's).
    /// `focus.*` resolves the same row so a dashboard can name
    /// `focus.altitude_m` instead of hard-coding `vessels.0`.
    pub focus_vessel_id: Option<u64>,
    /// The focused vessel's telemetry, duplicated — subscription
    /// paths `focus.<field>` work without knowing the id.
    pub focus: Option<SnapshotVessel>,
    /// The program layer — funds/science/contracts/colonies/crew/
    /// scripts. The old snapshot was write-only past vessels: an
    /// agent could `HireCrew`/`FoundColony` and never read a byte of
    /// it back (report-19 F3). Every field is subscribable for free
    /// through the dotted-path projection.
    pub program: ProgramSnapshot,
}

/// The program half of the snapshot — the read surface `sim.command`'s
/// write surface never had (report-19 F3).
#[derive(Clone, Debug, Serialize)]
pub struct ProgramSnapshot {
    /// `World::funds_micros` — the economy's funds counter.
    pub funds_micros: u64,
    /// Banked science points (`ScienceState::banked`).
    pub science_banked: f64,
    /// Open contract offers on the board.
    pub contract_offers: usize,
    /// Active (accepted, incomplete) contracts.
    pub contracts_active: usize,
    /// Researched tech nodes.
    pub tech_researched: usize,
    /// Crew roster size (`EconomyState::crew`).
    pub crew_roster: usize,
    /// Colony summaries — `id`, `body`, `state`, module/store counts.
    pub colonies: Vec<ColonyRow>,
    /// Bound scripts — id, bound vessel, enabled flag, strikes.
    pub scripts: Vec<ScriptRow>,
    /// Engaged autopilots by vessel id.
    pub autopilots: Vec<u64>,
    /// Site names with live state — the program's ground truth.
    pub sites: usize,
}

/// One colony's wire row.
#[derive(Clone, Debug, Serialize)]
pub struct ColonyRow {
    /// `ColonyId.0`.
    pub id: u64,
    /// The body it sits on.
    pub body: String,
    /// `ColonyState` rendered as a short tag.
    pub state: String,
    /// Installed module count.
    pub modules: usize,
    /// Live resource-kind count across stores.
    pub stores: usize,
}

/// One script's wire row.
#[derive(Clone, Debug, Serialize)]
pub struct ScriptRow {
    /// `ScriptId.0`.
    pub id: u64,
    /// The vessel it is bound to (`VesselId.0`).
    pub vessel: u64,
    /// `ScriptState::enabled`.
    pub enabled: bool,
    /// `ScriptState::strikes`.
    pub strikes: u32,
}

/// One vessel's wire row: the sim's telemetry snapshot flattened with
/// its control lease, so a subscription can read
/// `vessels.<id>.altitude_m` and `vessels.<id>.control_lease` off the
/// same object.
#[derive(Clone, Debug, Serialize)]
pub struct SnapshotVessel {
    /// The sim's telemetry (flattened into this object).
    #[serde(flatten)]
    pub telemetry: VesselTelemetry,
    /// §2.2.1's control lease — the interface tier needs it for its
    /// own pre-check (a fast, non-authoritative error; the drain's
    /// check is the backstop).
    pub control_lease: Lease,
}

impl TelemetrySnapshot {
    /// The publish point's build — observation only, one `Arc` per
    /// call, exactly the §2.12.3 "exactly one publish point" shape.
    pub fn from_world(world: &World) -> Self {
        Self::from_world_focus(world, None)
    }

    /// `from_world` carrying the host's focused vessel — `focus` is
    /// cloned out of `vessels` so `focus.*` paths resolve without the
    /// client knowing the id.
    pub fn from_world_focus(world: &World, focus: Option<u64>) -> Self {
        let vessels: BTreeMap<u64, SnapshotVessel> = world
            .vessels()
            .iter()
            .map(|(id, v)| {
                (
                    id.0,
                    SnapshotVessel {
                        telemetry: world.telemetry(*id),
                        control_lease: v.control_lease,
                    },
                )
            })
            .collect();
        let focus_row = focus.and_then(|f| vessels.get(&f).cloned());
        let focus_vessel_id = focus.filter(|f| vessels.contains_key(f));
        let program = ProgramSnapshot {
            funds_micros: world.funds_micros(),
            science_banked: world.science().banked,
            contract_offers: world.contracts().offers.len(),
            contracts_active: world.contracts().active.len(),
            tech_researched: world.tech().researched.len(),
            crew_roster: world.economy().crew.len(),
            colonies: world
                .colonies()
                .iter()
                .map(|(id, c)| ColonyRow {
                    id: id.0,
                    body: c.body.as_str().to_string(),
                    state: format!("{:?}", c.state),
                    modules: c.modules.len(),
                    stores: c.stores.len(),
                })
                .collect(),
            scripts: world
                .scripts()
                .iter()
                .map(|(id, sc)| ScriptRow {
                    id: id.0,
                    vessel: sc.vessel.0,
                    enabled: sc.enabled,
                    strikes: sc.strikes,
                })
                .collect(),
            autopilots: world.autopilots().keys().map(|v| v.0).collect(),
            sites: world.economy().sites.len(),
        };
        Self {
            tick: world.clock().ticks(),
            warp_level: world.warp_level(),
            vessels,
            focus_vessel_id,
            focus: focus_row,
            program,
        }
    }

    /// Resolve one field path against the serialized snapshot.
    /// `None` = the path does not exist (a subscription error, not a
    /// skipped field — the WS layer reports it). Serializes the whole
    /// snapshot per call — the single-field diagnostic form;
    /// [`Self::project`] is the batched path subscriptions take.
    pub fn field(&self, path: &str) -> Option<serde_json::Value> {
        Self::field_in(&serde_json::to_value(self).ok()?, path)
    }

    /// `path` against an already-serialized snapshot — `project`'s
    /// per-field step after the one `to_value` it pays per call.
    fn field_in(value: &serde_json::Value, path: &str) -> Option<serde_json::Value> {
        let mut cur = value;
        for seg in path.split('.') {
            cur = match cur {
                serde_json::Value::Object(m) => m.get(seg)?,
                serde_json::Value::Array(a) => {
                    let i: usize = seg.parse().ok()?;
                    a.get(i)?
                }
                _ => return None,
            };
        }
        Some(cur.clone())
    }

    /// Project a declared field list into `{path: value}` — the
    /// subscription wire payload (§2.12.2). `None`-valued paths are
    /// reported in `unknown`, never silently dropped. The snapshot
    /// serializes **once** per call, not once per field — a 64-field
    /// 64 Hz subscription is one `to_value` per tick, not 4,096/s.
    pub fn project(
        &self,
        fields: &[String],
    ) -> (serde_json::Map<String, serde_json::Value>, Vec<String>) {
        let mut out = serde_json::Map::new();
        let mut unknown = Vec::new();
        let Ok(root) = serde_json::to_value(self) else {
            // A snapshot that cannot serialize resolves nothing —
            // report every declared path unknown rather than guess.
            unknown.extend(fields.iter().cloned());
            return (out, unknown);
        };
        for f in fields {
            match Self::field_in(&root, f) {
                Some(v) => {
                    out.insert(f.clone(), v);
                }
                None => unknown.push(f.clone()),
            }
        }
        (out, unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_world_snapshot_projects_scalars() {
        let w = World::new();
        let snap = TelemetrySnapshot::from_world(&w);
        assert_eq!(snap.tick, 0);
        let fields = vec!["tick".to_string(), "warp_level".to_string()];
        let (out, unknown) = snap.project(&fields);
        assert!(unknown.is_empty());
        assert_eq!(out["tick"], serde_json::json!(0));
        assert!(snap.field("vessels.0.altitude_m").is_none());
    }
}
