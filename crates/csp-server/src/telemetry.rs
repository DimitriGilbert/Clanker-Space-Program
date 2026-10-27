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
    /// The resolved focused vessel — `None` only when the arena is
    /// empty. Resolution (see [`Self::from_world_focus`]): the host's
    /// or §2.2 `active_vessel`'s recorded focus while its vessel is
    /// live, else the newest live vessel. `focus.*` resolves the same
    /// row so a dashboard can name `focus.altitude_m` instead of
    /// hard-coding `vessels.0` — vessel ids are monotonic and never
    /// reused, so a recovery would brick a `vessels.N` dashboard
    /// forever (C28-F1).
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
    /// Surface sites — id, launch flag, live stockpile. The
    /// `ManufactureCraft` authoring surface (C27-F5): an agent reads
    /// the site ids here rather than guessing — this used to be a
    /// bare count, so the launch site's id was undiscoverable over
    /// the protocol.
    pub sites: Vec<SiteRow>,
    /// The installed part catalog, summarized for authoring — the
    /// `CraftDef`/`build_resources` inputs a `ManufactureCraft`
    /// command needs (C27-F5): id, cost, dry mass, the tech gate,
    /// and the authored BOM per part. Config data republished per
    /// snapshot; the table is small (one row per `data/parts/*.ron`)
    /// and there is no other config-read channel to hang it on.
    pub parts: Vec<PartRow>,
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

/// One surface site's wire row — the `ManufactureCraft` `site`
/// argument's discoverable form (C27-F5): the site id, its def's
/// launch flag (only launch sites accept build orders), and the
/// live stockpile the order spends.
#[derive(Clone, Debug, Serialize)]
pub struct SiteRow {
    /// The `SiteId` string — `ManufactureCraft.site`'s value.
    pub id: String,
    /// The body the site sits on (`""` when the arena row outlives
    /// its def — a config skew the row reports rather than hides).
    pub body: String,
    /// `true` = the body's launch site (`ManufactureCraft` requires it).
    pub launch: bool,
    /// Live stockpile `{resource: units}` — what a build order
    /// (and `RecoverVessel` credit) spends.
    pub inventory: BTreeMap<String, f64>,
}

/// One part-catalog entry's wire row — the authoring summary an
/// agent needs to compose a `CraftDef`/`build_resources` for
/// `ManufactureCraft` (C27-F5), not the whole `PartDef`.
#[derive(Clone, Debug, Serialize)]
pub struct PartRow {
    /// The `PartId` string — a `CraftDef::part_ids` member's value.
    pub id: String,
    /// `PartCategory` rendered as its debug tag.
    pub category: String,
    /// Dry mass at `scale = 1` (kg).
    pub mass_dry_kg: f64,
    /// Funds cost at `scale = 1`, stock variant.
    pub cost_funds: f64,
    /// The `TechNodeId` the tech tree must have researched — the
    /// spawn/manufacture gate's key.
    pub tech_requirement: String,
    /// The authored bill of materials `{resource: units}` — the
    /// per-part share of `build_resources`.
    pub build_resources: BTreeMap<String, f64>,
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
    /// Focus starts from §2.2's `active_vessel` (the focus record
    /// `SetActiveVessel` maintains — the sim scrubs it when the named
    /// vessel leaves the arena); the newest-live fallback in
    /// [`Self::from_world_focus`] still applies when it is `None`.
    pub fn from_world(world: &World) -> Self {
        Self::from_world_focus(world, world.active_vessel().map(|v| v.0))
    }

    /// `from_world` carrying the host's focused vessel — `focus` is
    /// cloned out of `vessels` so `focus.*` paths resolve without the
    /// client knowing the id. A passed focus that is `None` or names
    /// a dead id falls back to the newest live vessel (the last
    /// `BTreeMap` key — ids are monotonic, never reused): a headless
    /// host has no focus producer at all, and publishing `focus:
    /// null` while vessels fly would leave every `focus.*`
    /// subscription dark — the same brick the `vessels.0` hard-coding
    /// was (C28-F1).
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
        let focus_vessel_id = focus
            .filter(|f| vessels.contains_key(f))
            .or_else(|| vessels.keys().next_back().copied());
        let focus_row = focus_vessel_id.and_then(|f| vessels.get(&f).cloned());
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
            sites: world
                .economy()
                .sites
                .iter()
                .map(|(id, state)| {
                    // Geography/launch flag live in the def table
                    // (construction config); the arena row carries
                    // the live stockpile. A row without a def is a
                    // config skew — report it body-less rather than
                    // drop it silently.
                    let def = world.site_defs().get(id);
                    SiteRow {
                        id: id.as_str().to_string(),
                        body: def.map(|d| d.body.as_str().to_string()).unwrap_or_default(),
                        launch: def.is_some_and(|d| d.launch),
                        inventory: state
                            .inventory
                            .iter()
                            .map(|(r, u)| (r.as_str().to_string(), *u))
                            .collect(),
                    }
                })
                .collect(),
            parts: world
                .parts()
                .iter()
                .map(|(id, def)| PartRow {
                    id: id.as_str().to_string(),
                    category: format!("{:?}", def.category),
                    mass_dry_kg: def.mass_dry_kg,
                    cost_funds: def.cost_funds,
                    tech_requirement: def.tech_requirement.as_str().to_string(),
                    build_resources: def
                        .build_resources
                        .iter()
                        .map(|(r, u)| (r.as_str().to_string(), *u))
                        .collect(),
                })
                .collect(),
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
#[allow(clippy::unwrap_used, clippy::expect_used)] // test assertions
mod tests {
    use super::*;
    use csp_sim::{SimCommand, SourceClass, VesselId};

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

    /// The flight-launch fixture world with two spawned rails vessels
    /// — `VesselId(0)` and `VesselId(1)` (ids are monotonic and never
    /// reused, the property C28-F1's re-key depends on).
    fn two_vessel_world() -> World {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/test-fixtures/flight-launch");
        let recipe = csp_bench::scenarios::find("flight-launch").expect("recipe");
        let mut w = (recipe.initial)(&dir).expect("fixture world");
        // csp-server never names csp-data's ids/elements types — the
        // spawn decodes through the same serde_json shape `sim.command`
        // takes on the wire.
        let spawn: SimCommand = serde_json::from_value(serde_json::json!({
            "SpawnVessel": {
                "parent": "terrin",
                "elements": {
                    "semi_major_axis_m": 700_000.0,
                    "eccentricity": 0.0,
                    "inclination_rad": 0.0,
                    "ascending_node_rad": 0.0,
                    "arg_periapsis_rad": 0.0,
                    "mean_anomaly_epoch_rad": 0.0,
                    "period_s": 1958.1284360312443
                }
            }
        }))
        .expect("spawn command json");
        for _ in 0..2 {
            w.enqueue(SourceClass::Ui, 0, spawn.clone());
        }
        let t = w.clock().ticks();
        w.step_frame(t + 1).expect("spawn step");
        assert!(w.vessels().contains_key(&VesselId(0)));
        assert!(w.vessels().contains_key(&VesselId(1)));
        w
    }

    /// C28-F1: a headless host has no focus producer — `focus.*`
    /// must still resolve, against the newest live vessel, or the
    /// shipped dashboard would publish `focus: null` while vessels fly.
    #[test]
    fn focus_falls_back_to_newest_live_vessel() {
        let w = two_vessel_world();
        assert_eq!(w.active_vessel(), None);
        let snap = TelemetrySnapshot::from_world(&w);
        assert_eq!(snap.focus_vessel_id, Some(1));
        assert!(snap.field("focus.altitude_m").is_some());
        assert!(snap.field("focus.control_lease").is_some());
        let fields = vec![
            "focus_vessel_id".to_string(),
            "focus.altitude_m".to_string(),
        ];
        let (out, unknown) = snap.project(&fields);
        assert!(unknown.is_empty(), "{unknown:?}");
        assert_eq!(out["focus_vessel_id"], serde_json::json!(1));
    }

    /// The recorded §2.2 `active_vessel` wins over the newest-live
    /// fallback while its vessel is in the arena.
    #[test]
    fn focus_tracks_the_active_vessel_record() {
        let mut w = two_vessel_world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetActiveVessel {
                vessel: Some(VesselId(0)),
            },
        );
        let t = w.clock().ticks();
        w.step_frame(t + 1).expect("step");
        let snap = TelemetrySnapshot::from_world(&w);
        assert_eq!(snap.focus_vessel_id, Some(0));
        assert!(snap.field("focus.altitude_m").is_some());
    }

    /// C27-F5: `program.sites` carries the discoverable authoring
    /// surface — the site ids `ManufactureCraft` names, the launch
    /// flag its apply gates on, and the live stockpile — not a bare
    /// count. The fixture's two-site set (launch pad + downrange
    /// field) is the assertion.
    #[test]
    fn program_sites_expose_ids_launch_and_inventory() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/test-fixtures/flight-launch");
        let recipe = csp_bench::scenarios::find("flight-launch").expect("recipe");
        let w = (recipe.initial)(&dir).expect("fixture world");
        let snap = TelemetrySnapshot::from_world(&w);
        let launch = snap
            .program
            .sites
            .iter()
            .find(|s| s.id == "terrin-launch")
            .expect("the launch site must be listed");
        assert!(launch.launch, "terrin-launch is the launch site");
        assert_eq!(launch.body, "terrin");
        let downrange = snap
            .program
            .sites
            .iter()
            .find(|s| s.id == "terrin-downrange")
            .expect("downrange site listed");
        assert!(!downrange.launch);
        // The rows stay subscribable through the dotted path.
        assert!(snap.field("program.sites").is_some());
    }

    /// C27-F5: `program.parts` carries the catalog summary a
    /// `CraftDef`/`build_resources` author needs — id, mass, cost,
    /// the tech gate, and the authored BOM — read off the real
    /// `data/parts/` table.
    #[test]
    fn program_parts_expose_the_catalog_summary() {
        let mut w = World::new();
        let loaded = csp_data::load_parts(
            &std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../data/parts"),
        )
        .expect("part catalog loads");
        w.set_parts(
            loaded
                .parts
                .iter()
                .map(|p| (p.def.id.clone(), p.def.clone()))
                .collect(),
        );
        let snap = TelemetrySnapshot::from_world(&w);
        assert!(!snap.program.parts.is_empty());
        let pod = snap
            .program
            .parts
            .iter()
            .find(|p| p.id == "csp.pod.mk1")
            .expect("the pod must be listed");
        assert_eq!(pod.category, "Control");
        assert_eq!(pod.mass_dry_kg, 800.0);
        assert_eq!(pod.cost_funds, 2400.0);
        assert!(!pod.tech_requirement.is_empty());
        assert_eq!(pod.build_resources.get("metal"), Some(&60.0));
    }

    /// The C28-F1 regression shape: the dashboard's keyed vessel
    /// leaves the arena (recovery — `TerminateVessel` stands in,
    /// same `remove_vessel` path without the landed precondition) and
    /// every `focus.*` path must keep resolving against a live vessel
    /// instead of going "unknown field" forever.
    #[test]
    fn focus_rekeys_when_the_focused_vessel_leaves() {
        let mut w = two_vessel_world();
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::SetActiveVessel {
                vessel: Some(VesselId(0)),
            },
        );
        let t = w.clock().ticks();
        w.step_frame(t + 1).expect("step");
        // `TerminateVessel` is §2.2.1 control-gated — `Ui` preempts
        // (the player's removal verb); an `External` source would
        // need the vessel's lease first.
        w.enqueue(
            SourceClass::Ui,
            0,
            SimCommand::TerminateVessel {
                vessel: VesselId(0),
            },
        );
        let t = w.clock().ticks();
        w.step_frame(t + 1).expect("step");
        assert_eq!(w.active_vessel(), None, "removal scrubs the record");
        let snap = TelemetrySnapshot::from_world(&w);
        // Re-keyed to the surviving vessel — and `vessels.0` is gone
        // for good (monotonic ids), the failure the focus indirection
        // exists to hide.
        assert_eq!(snap.focus_vessel_id, Some(1));
        assert!(snap.field("focus.altitude_m").is_some());
        assert!(snap.field("vessels.0.altitude_m").is_none());
        // The program half stays subscribable through the same event.
        let fields = vec![
            "focus.altitude_m".to_string(),
            "program.funds_micros".to_string(),
        ];
        let (_out, unknown) = snap.project(&fields);
        assert!(unknown.is_empty(), "{unknown:?}");
    }
}
