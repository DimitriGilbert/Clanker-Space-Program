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
        let vessels = world
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
        Self {
            tick: world.clock().ticks(),
            warp_level: world.warp_level(),
            vessels,
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
