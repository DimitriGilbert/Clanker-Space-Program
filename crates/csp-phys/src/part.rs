//! Per-part resolved data — the second `weld` argument of §2.4's
//! `csp_phys::weld(part_graph, &[WeldPart])`.
//!
//! [`WeldPart`] is exactly the *weld-consumed view* of §3.1's
//! `EffectivePart = resolve(BuiltPart) ∘ wear(PartUsage)`: the numbers
//! Stage 1 needs, already resolved. The name was split when the §3.1
//! resolver landed — `crate::resolve::EffectivePart` is the resolved
//! craft part; `WeldPart` is its `weld_input` field, the projection
//! the weld consumes (see `resolve.rs`'s naming-reconciliation note).
//! Nothing below derives stats from `PartDef`s, upgrades, or wear —
//! that is the resolver's job.

use serde::{Deserialize, Serialize};

use csp_core::{DVec3, StableDiscriminant};
use csp_data::{AeroDef, ThermalDef};

use crate::tensor::SymMat3;

/// What a force site emits — the `kind` of §2.4's
/// `ForceSite { local_pos, local_dir, kind }`.
///
/// Site-producing modules are resolved by M3's effective-stat pipeline;
/// the weld only transforms the sites it is handed. Discriminants are
/// append-only `u16`s — the declared width [`StableDiscriminant`]
/// hashes — and stable: they will be what the canonical hash encodes
/// once vessels enter `state_hash` (§7.1 rule 7).
#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ForceSiteKind {
    /// Engine nozzle: thrust along `local_dir` applied at `local_pos`.
    Thrust = 0,
    /// Per-part aerodynamic application point (§2.4.1 sums per-part
    /// forces "at the part's position").
    Aero = 1,
    /// RCS jet: thrust along `local_dir`.
    Rcs = 2,
}

impl StableDiscriminant for ForceSiteKind {
    fn discriminant(&self) -> u16 {
        *self as u16
    }
}

/// `ForceSite { local_pos, local_dir, kind }` verbatim from §2.4, plus
/// `rated_n` — the site's rated force in newtons, the magnitude channel
/// the §2.9 force model needs (`F = throttle · rated_n` along
/// `local_dir` for `Thrust`/`Rcs` sites). §2.4 lists the geometry the
/// weld must carry; the rating is resolved per-site by the same M3
/// resolver that produces the site, and the weld transports it
/// unmodified like it does `kind`. For `Aero` sites `rated_n` is
/// unused — aero force comes from the part's §2.4.1 table, not the
/// site record.
///
/// Frame convention is contextual and documented at each use:
/// part-local inside [`WeldPart`], body-frame inside
/// [`crate::WeldedBody`]. `local_dir` is a unit direction supplied by
/// the resolver — the weld rotates but never renormalizes it (a unit
/// quaternion acting on a unit vector stays unit to f64 rounding, and
/// renormalizing would only add noise).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForceSite {
    pub local_pos: DVec3,
    pub local_dir: DVec3,
    pub kind: ForceSiteKind,
    /// Rated force in newtons — finite, `≥ 0` (the weld's input gate).
    /// Only `Thrust`/`Rcs` sites read it.
    pub rated_n: f64,
}

impl ForceSite {
    pub const fn new(
        local_pos: DVec3,
        local_dir: DVec3,
        kind: ForceSiteKind,
        rated_n: f64,
    ) -> Self {
        Self {
            local_pos,
            local_dir,
            kind,
            rated_n,
        }
    }
}

/// A part's collision shape as resolved data: already-convex geometry
/// in part-local frame. The weld transforms the vertex set into body
/// frame to form one hull of the merged compound collider (§2.4 Stage
/// 1); `csp-phys`'s backend materializes `parry3d-f64`
/// `ConvexPolyhedron`s from those point sets when contact solving
/// lands (sub-phase B).
///
/// Variants are the primitive shapes `ColliderDef` (§3.1) will resolve
/// to; `Points` is the escape hatch for authored convex sets. Vertices
/// are the *extreme points* of a convex region — interior points are
/// harmless but wasted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ColliderShape {
    /// Vertex set of a convex polytope, part-local frame.
    Points(Vec<DVec3>),
    /// Axis-aligned box by half-extents — 8 extreme points.
    Box { half_extents: DVec3 },
}

impl ColliderShape {
    /// The convex-hull vertex set in part-local frame, in a fixed
    /// enumeration order.
    pub fn vertices(&self) -> Vec<DVec3> {
        match self {
            Self::Points(pts) => pts.clone(),
            Self::Box { half_extents: h } => {
                let mut v = Vec::with_capacity(8);
                for sx in [-1.0, 1.0] {
                    for sy in [-1.0, 1.0] {
                        for sz in [-1.0, 1.0] {
                            v.push(DVec3::new(h.x * sx, h.y * sy, h.z * sz));
                        }
                    }
                }
                v
            }
        }
    }

    /// All vertices finite — the weld's input gate.
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Points(pts) => pts.iter().all(|p| p.is_finite()),
            Self::Box { half_extents: h } => h.is_finite(),
        }
    }
}

/// One part's resolved mass/shape/site data — everything Stage 1 of the
/// §2.4 weld consumes, nothing more.
///
/// Field semantics are part-local: `com` and `inertia` are about/in the
/// part's own local frame, `force_sites` and `collider` are part-local
/// geometry. `inertia` is the full symmetric tensor about `com`, so the
/// resolver may fill it from a primitive formula or a polyhedral
/// integration — the weld composes either identically.
///
/// `mass_kg` is the part's *current* total mass: dry mass plus
/// quantized resource contents. When a tank's 1/1024-of-capacity
/// quantum flips (§2.4 inertia-cache rule), the caller rebuilds the
/// affected `WeldPart`s and re-welds — the cache is a pure
/// function of currently-serialized state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WeldPart {
    /// Total part mass, kg. Finite and `≥ 0`; a zero-mass part is legal
    /// *as weld input* (massless structural/marker geometry) but the
    /// vessel total must be `> 0`. Note the §3.1 resolver is stricter
    /// — it refuses to *resolve* a def to zero mass
    /// (`ResolveError::InvalidStat`): a part whose resolved stats are
    /// massless is bad authored data, while a hand-authored `WeldPart`
    /// at zero stays legal weld input.
    pub mass_kg: f64,
    /// Centre of mass in part-local frame.
    pub com: DVec3,
    /// Inertia tensor about `com`, part-local frame, kg·m². Must be
    /// positive-semidefinite; `SymMat3::ZERO` for a point mass.
    pub inertia: SymMat3,
    /// Convex collision shape, part-local.
    pub collider: ColliderShape,
    /// Force sites in part-local frame, in resolver order.
    pub force_sites: Vec<ForceSite>,
    /// §3.1 `aero`: the authored `Cd`/`Cl`-vs-Mach curves and
    /// reference length. Stage 1 ignores it; the §2.4.1 Stage-2
    /// voxelizer ([`crate::aero`]) consumes it when building the
    /// craft's directional tables. Hand-authored `WeldPart`s may carry
    /// empty curves — the collider still occludes rays, it simply
    /// produces no force (`eval_aero_curve` on an empty curve is 0).
    pub aero: AeroDef,
    /// §3.1 `thermal`: the resolved lumped-capacity parameters —
    /// `heat_capacity_j_per_k` (mass·cp), `max_temp_k` (the §3.4
    /// overheat lethality threshold), `emissivity` ε for the
    /// Stefan–Boltzmann skin term, and `conduction_w_per_k` coupling
    /// to attached neighbours. Stage 1 consumes it to build the
    /// welded body's [`crate::thermal::ThermalNet`]; hand-authored
    /// `WeldPart`s must carry finite, in-range values (the weld's
    /// input gate).
    pub thermal: ThermalDef,
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// `Box::vertices` enumerates the 8 corners in a fixed sign order:
    /// x-major, then y, then z — (−−−, −−+, −+−, −++, +−−, +−+, ++−,
    /// +++). The order is part of the contract (canonicalization then
    /// re-sorts, but resolvers may rely on it).
    #[test]
    fn box_vertices_enumerate_in_fixed_order() {
        let h = DVec3::new(1.0, 2.0, 3.0);
        let v = ColliderShape::Box { half_extents: h }.vertices();
        let expected: Vec<DVec3> = [-1.0, 1.0]
            .into_iter()
            .flat_map(|sx| {
                [-1.0, 1.0].into_iter().flat_map(move |sy| {
                    [-1.0, 1.0]
                        .into_iter()
                        .map(move |sz| DVec3::new(h.x * sx, h.y * sy, h.z * sz))
                })
            })
            .collect();
        assert_eq!(v, expected);
        assert_eq!(v.len(), 8);
        // First/last corners pinned literally, not just by construction.
        assert_eq!(v[0], DVec3::new(-1.0, -2.0, -3.0));
        assert_eq!(v[7], DVec3::new(1.0, 2.0, 3.0));
    }

    /// `Points::vertices` returns the authored set verbatim — ordering
    /// is the resolver's, dedup/sort is the weld's `canonicalize` job.
    #[test]
    fn points_vertices_pass_through() {
        let pts = vec![
            DVec3::new(2.0, 0.0, 0.0),
            DVec3::new(-1.0, 0.5, 0.25),
            DVec3::new(2.0, 0.0, 0.0), // duplicate preserved pre-weld
        ];
        assert_eq!(ColliderShape::Points(pts.clone()).vertices(), pts);
    }

    /// Every `is_valid` failure mode: non-finite point, non-finite box
    /// half-extent; valid shapes pass.
    #[test]
    fn collider_shape_is_valid() {
        assert!(ColliderShape::Points(vec![DVec3::ZERO]).is_valid());
        assert!(ColliderShape::Points(vec![]).is_valid());
        assert!(!ColliderShape::Points(vec![DVec3::new(f64::NAN, 0.0, 0.0)]).is_valid());
        assert!(
            !ColliderShape::Points(vec![DVec3::ZERO, DVec3::new(0.0, f64::INFINITY, 0.0)])
                .is_valid()
        );
        assert!(
            ColliderShape::Box {
                half_extents: DVec3::new(0.5, 0.5, 0.5)
            }
            .is_valid()
        );
        assert!(
            !ColliderShape::Box {
                half_extents: DVec3::new(0.5, f64::NEG_INFINITY, 0.5)
            }
            .is_valid()
        );
    }

    /// The declared `u16` discriminants are the hash encoding — pinned
    /// so a renumbering is a loud test failure, not a silent hash drift.
    #[test]
    fn force_site_kind_discriminants_are_stable() {
        assert_eq!(ForceSiteKind::Thrust.discriminant(), 0);
        assert_eq!(ForceSiteKind::Aero.discriminant(), 1);
        assert_eq!(ForceSiteKind::Rcs.discriminant(), 2);
    }
}
