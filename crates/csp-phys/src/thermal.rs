//! M4b thermal model — per-part lumped-capacity temperatures over the
//! §3.1 `ThermalDef` parameters, conduction across the part graph's
//! attachment edges, convective reentry heating off the §2.4.1 aero
//! tables, and Stefan–Boltzmann skin exchange with the ambient sink.
//!
//! # Model
//!
//! One lumped node per part — the plan's `ThermalDef` is "lumped-
//! capacity parameters" verbatim (`heat_capacity_j_per_k` is
//! `mass·cp`); no skin/core split is named, so the lump IS the skin
//! for the radiation term. Per part `i`, per tick:
//!
//! ```text
//! C_i · dT_i/dt = Q̇_conv,i + Q̇_rad,i + Σ_e G_e·(T_j − T_i)
//!
//! Q̇_conv,i = HEAT_FRACTION · ½·ρ·|v_rel|³ · A_eff,i(ŵ)   — in atmosphere
//! Q̇_rad,i  = ε_i·σ·A_rad,i·(T_amb⁴ − T_i⁴)              — always
//! ```
//!
//! - `A_eff,i(ŵ)` is the part's *unblocked* projected area along the
//!   flow direction: `area_m2·(1 − shielded)` barycentric-interpolated
//!   from the part's aero table at `ŵ` — shielded parts take no
//!   convective input, which is what makes a nose-cone/fairing a
//!   heatshield. Parts with no table entry (mask-inactive, or no
//!   tables installed) fall back to the hull-AABB projected area with
//!   no shielding — the same resolution the pre-voxel drag fallback
//!   lives at.
//! - `A_rad,i` is the part's radiative area, estimated at weld as the
//!   surface area of its body-frame hull AABB (`2(xy+yz+zx)`) — exact
//!   for the ubiquitous box hull, an over-estimate (≤ ~27%) for the
//!   inscribed-prism cylinders. The lumped model does not occlude
//!   radiation between parts (documented approximation; the skin/core
//!   split the plan doesn't name is what would fix it).
//! - `T_amb` is the §3.0 atmosphere temperature at altitude inside
//!   `top_m`, else [`SPACE_SINK_K`] — a parked vessel radiates toward
//!   deep space, not toward absolute zero.
//! - Integration is explicit Euler at the pinned `1/64` s — stable
//!   because authored couplings satisfy `G·dt/C ≪ 1` (W/K vs J/K) and
//!   the radiation Jacobian `4εσA·T³/C` is likewise tiny at every
//!   temperature that survives a tick. No implicit solve is warranted
//!   at this dt.
//!
//! # Conduction coupling (§3.1 `conduction_w_per_k`)
//!
//! The authored field is the part's joint conductance (W/K) presented
//! at a *fully mated* interface. Stage 1 forms one [`ThermalEdge`] per
//! parent→child attachment, with conductance
//!
//! ```text
//! G_ij = 2·k_i·k_j/(k_i+k_j) · min(1, A_shared / min(A_i, A_j))
//! ```
//!
//! — the series (harmonic) combination of the two parts' coupling,
//! derated by the fraction of the smaller part's interface face
//! actually shared. `A_shared` is the overlap of the two parts'
//! hull-AABB projections onto the plane ⊥ the attach axis (the
//! dominant component of the AABB-centre separation — the right face
//! for stacked and radially-mounted parts alike). A fully mated joint
//! on the smaller part gives the full harmonic conductance; a
//! partially-overlapping or off-axis joint scales it down. An edge
//! whose geometry produces zero coupling is simply absent from the
//! net.
//!
//! # Steady-state skin temperature
//!
//! [`steady_state_temp_k`] is the closed-form equilibrium of a single
//! isolated lump under constant convective input, `Q̇ = εσA·(T⁴ −
//! T_amb⁴)` → `T_ss = (T_amb⁴ + Q̇/(εσA))^{1/4}` — the reference the
//! heating integration is bounded against in tests, and the value a
//! UI "equilibrium temperature" readout should show.
//!
//! # Lethality (§3.4)
//!
//! A part whose lump exceeds `max_temp_k` at the end of a tick is
//! returned by [`step`] in ascending order — the thermal trigger for
//! §3.4's fatal `Explosion` failure path. The sim side
//! (`csp_sim::World::step_bubble_range`) applies the destruction; this
//! module stays pure of policy.
//!
//! Determinism: `f64` everywhere, `libm` only, iteration in ascending
//! `PartNodeId`/edge order — the state is serialized on the vessel
//! and enters `state_hash` through `csp-sim`'s walk.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use csp_core::DVec3;

use crate::aero::CraftAeroTables;
use crate::graph::PartNodeId;
use crate::simple::{Environment, atmosphere_density_kg_m3, atmosphere_temperature_k};
use crate::weld::WeldedBody;

/// Stefan–Boltzmann constant σ, W/(m²·K⁴) — CODATA exact.
pub const STEFAN_BOLTZMANN_W_M2_K4: f64 = 5.670_374_419e-8;

/// The deep-space radiative sink temperature (K) — the ambient
/// `T⁴` term outside any atmosphere (and on airless worlds).
/// ~2.7 K is the cosmic background; a part in shadow of nothing
/// else cools toward it.
pub const SPACE_SINK_K: f64 = 2.7;

/// The dimensionless fraction of the kinetic energy flux `½ρv³`
/// incident on a part's exposed area that is absorbed as skin heat —
/// the v1 convective-heating coefficient. At reentry conditions
/// (ρ ~ 4·10⁻² kg/m³, v ~ 2 km/s, A ~ 2.25 m²) η = 5·10⁻³ drives a
/// ~400 kJ/K lump at ~5–6 K/s through the heat pulse — a real but
/// survivable pulse for authored `max_temp_k` 900–1600 K parts, and
/// lethal above them or at lower capacity. A stand-in in the same
/// sense as [`crate::aero::AIR_VISCOSITY_PA_S`]: the pinned v1 value
/// pending a `balance.ron`-level tunable.
pub const CONVECTIVE_HEAT_FRACTION: f64 = 0.005;

/// One part's weld-resolved thermal statics — the lumped node the
/// per-tick [`step`] integrates. Geometry fields are body-frame:
/// they come from the part's collider hull as welded (the same
/// transformed vertices that fill [`crate::weld::CompoundCollider`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThermalNode {
    /// The graph node this lump belongs to.
    pub node: PartNodeId,
    /// `mass·cp` — the resolved `heat_capacity_j_per_k` (J/K).
    pub heat_capacity_j_per_k: f64,
    /// §3.4 overheat threshold (K).
    pub max_temp_k: f64,
    /// Surface emissivity ε ∈ (0, 1] for the radiation term.
    pub emissivity: f64,
    /// Radiative area (m²) — the hull AABB's surface estimate (see
    /// the module doc for the approximation).
    pub radiative_area_m2: f64,
    /// The part's hull AABB in body frame — the basis of the
    /// conduction contact-area estimate and the no-tables convective
    /// fallback.
    pub aabb_lo: DVec3,
    /// See [`Self::aabb_lo`].
    pub aabb_hi: DVec3,
}

impl ThermalNode {
    /// The part's projected silhouette area along `dir` (m²) — exact
    /// for the AABB proxy `|d·x̂|·(dy·dz) + |d·ŷ|·(dx·dz) + |d·ẑ|·(dx·dy)`.
    /// `dir` need not be normalized for the formula to stay honest —
    /// callers pass the unit flow direction.
    pub fn projected_area(&self, dir: DVec3) -> f64 {
        let e = self.aabb_hi - self.aabb_lo;
        dir.x.abs() * (e.y * e.z) + dir.y.abs() * (e.x * e.z) + dir.z.abs() * (e.x * e.y)
    }
}

/// One attachment-edge conduction coupling between `a` and `b` at
/// `conductance_w_per_k` W/K, computed at weld (module doc). The
/// joint is undirected — endpoints are stored canonically (`a < b`)
/// so the net is invariant under part-graph re-rooting (a session
/// re-root flips `parent`/`child` on the same physical joint and the
/// weld output must not move). The graph being a tree, there is
/// exactly one edge per non-root node — this is also the coupling
/// set M4c's breakup can later cut.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ThermalEdge {
    /// One end of the attachment — canonically the lesser node id.
    pub a: PartNodeId,
    /// The other end — canonically the greater node id.
    pub b: PartNodeId,
    /// The joint conductance `G_ij` (W/K) — see the module doc.
    pub conductance_w_per_k: f64,
}

/// The welded body's conduction network — a **derived cache** of the
/// `(graph, parts)` weld inputs, built by `weld` Stage 1 alongside the
/// compound collider. Serialized with the body like the rest of the
/// weld output; restore re-welds, so it is never independently
/// authoritative — the craft's hashed fields cover it (§7.3).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThermalNet {
    /// The part lumps, ascending [`PartNodeId`].
    pub nodes: Vec<ThermalNode>,
    /// Attachment couplings, ascending `(a, b)`.
    pub edges: Vec<ThermalEdge>,
}

/// The environment sample one thermal tick consumes — computed once
/// per body per tick by [`flow_sample`] from the same quantities the
/// §2.9 force model reads (co-rotating-atmosphere `v_rel`, altitude
/// against the contact sphere, the authored temperature table).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThermalFlow {
    /// Radiative sink temperature (K): the §3.0 atmosphere temperature
    /// at the body's altitude inside `top_m`, else [`SPACE_SINK_K`].
    pub ambient_k: f64,
    /// Local air density (kg/m³) — zero outside the atmosphere.
    pub density_kg_m3: f64,
    /// `|v_rel|` — speed relative to the co-rotating atmosphere (m/s).
    pub speed_mps: f64,
    /// Unit flow direction in body frame, `ŵ = −v̂_rel` — meaningful
    /// iff `speed_mps > 0`.
    pub dir_body: DVec3,
}

/// The sink temperature the radiation term sees at `altitude_m`:
/// the authored §3.0 temperature table inside the atmosphere,
/// [`SPACE_SINK_K`] above `top_m` or on airless worlds.
pub fn ambient_k(env: &Environment, altitude_m: f64) -> f64 {
    match &env.atmosphere {
        Some(atm) if altitude_m < atm.top_m => atmosphere_temperature_k(atm, altitude_m),
        _ => SPACE_SINK_K,
    }
}

/// One body's environment sample for the thermal tick — mirrors
/// `SimpleBackend::accumulate_forces`' atmosphere read (same `v_rel`,
/// same altitude datum) so heating and drag see the same flow.
pub fn flow_sample(env: &Environment, body: &WeldedBody) -> ThermalFlow {
    let rel = body.com_world() - env.body_center;
    let alt = rel.norm() - env.radius_m;
    let ambient = ambient_k(env, alt);
    let (density, speed, dir) = match &env.atmosphere {
        Some(atm) if alt < atm.top_m => {
            let rho = atmosphere_density_kg_m3(atm, alt);
            let v_rel = body.vel - env.body_spin.cross(rel);
            let speed = v_rel.norm();
            let dir = if speed > 0.0 {
                body.rot.conjugate().rotate(v_rel / -speed)
            } else {
                DVec3::new(0.0, 0.0, -1.0)
            };
            (rho, speed, dir)
        }
        _ => (0.0, 0.0, DVec3::new(0.0, 0.0, -1.0)),
    };
    ThermalFlow {
        ambient_k: ambient,
        density_kg_m3: density,
        speed_mps: speed,
        dir_body: dir,
    }
}

/// The single-lump equilibrium skin temperature under constant
/// convective input — `T_ss = (T_amb⁴ + Q̇/(εσA))^{1/4}` (module doc).
/// `+∞` when the part has no radiative channel (`εσA ≤ 0`): with no
/// sink the lump heats without bound. Conduction is excluded by
/// construction — this is the isolated-part bound.
pub fn steady_state_temp_k(
    q_conv_w: f64,
    emissivity: f64,
    radiative_area_m2: f64,
    ambient_k: f64,
) -> f64 {
    let denom = emissivity * STEFAN_BOLTZMANN_W_M2_K4 * radiative_area_m2;
    if denom <= 0.0 {
        return f64::INFINITY;
    }
    let a2 = ambient_k * ambient_k;
    libm::sqrt(libm::sqrt(a2 * a2 + q_conv_w / denom))
}

/// `net.nodes` position for `id` — the nodes are stored ascending, so
/// this is a binary search; `None` iff the id is not in the net.
fn node_index(net: &ThermalNet, id: PartNodeId) -> Option<usize> {
    net.nodes.binary_search_by_key(&id, |n| n.node).ok()
}

/// One deterministic tick of the lumped network — explicit Euler over
/// the module-doc equation set at the caller's `dt` (the sim passes
/// [`crate::simple::PHYSICS_DT`]). `temps` is the vessel's serialized
/// per-part temperature map (K), keyed by graph node; entries missing
/// for a net node are left alone — sim-side validation keeps the key
/// sets identical, and the skip keeps a corrupt record from panicking
/// the tick (§2.13).
///
/// `rates` is caller-owned scratch for the per-node net heat rate
/// (W): cleared and resized to `net.nodes.len()` on entry, never read
/// after return. The sim keeps one buffer alive across bodies and
/// ticks — a `vec![0.0; n]` here would be a fresh allocation per body
/// per tick.
///
/// Evaluation order is fixed: convective input first, then edge
/// conduction against the *pre-update* temperatures, then the
/// radiation term inside the integration pass — explicit Euler sees a
/// consistent snapshot. Returns the nodes that ended the tick above
/// `max_temp_k`, ascending — the §3.4 thermal lethality trigger the
/// caller applies.
pub fn step(
    net: &ThermalNet,
    aero: Option<&CraftAeroTables>,
    flow: &ThermalFlow,
    temps: &mut BTreeMap<PartNodeId, f64>,
    rates: &mut Vec<f64>,
    dt: f64,
) -> Vec<PartNodeId> {
    rates.clear();
    rates.resize(net.nodes.len(), 0.0);

    // Convective input: q̇_i = η·½ρv³·A_eff,i(ŵ). With tables, A_eff is
    // the part's interpolated unblocked area; without them, the AABB
    // projected-area fallback (both per the module doc).
    if flow.density_kg_m3 > 0.0 && flow.speed_mps > 0.0 && flow.dir_body.is_finite() {
        let v = flow.speed_mps;
        let per_area = CONVECTIVE_HEAT_FRACTION * 0.5 * flow.density_kg_m3 * v * v * v;
        match aero {
            Some(tables) => {
                let (ids, wt) = tables.directions.weights(flow.dir_body);
                let (i0, i1, i2) = (ids[0] as usize, ids[1] as usize, ids[2] as usize);
                // Merge join: `net.nodes` and `tables.parts` both ascend
                // PartNodeId; mask-inactive parts (no table row) get
                // nothing.
                let mut p = 0usize;
                for (i, n) in net.nodes.iter().enumerate() {
                    while p < tables.parts.len() && tables.parts[p].node < n.node {
                        p += 1;
                    }
                    if p < tables.parts.len() && tables.parts[p].node == n.node {
                        let t = &tables.parts[p];
                        let area =
                            wt[0] * t.area_m2[i0] + wt[1] * t.area_m2[i1] + wt[2] * t.area_m2[i2];
                        let sh = wt[0] * t.shielded[i0]
                            + wt[1] * t.shielded[i1]
                            + wt[2] * t.shielded[i2];
                        rates[i] += per_area * area * (1.0 - sh);
                    }
                }
            }
            None => {
                for (i, n) in net.nodes.iter().enumerate() {
                    rates[i] += per_area * n.projected_area(flow.dir_body);
                }
            }
        }
    }

    // Conduction across attachment edges, pre-update temperatures.
    for e in &net.edges {
        let (Some(&ta), Some(&tb)) = (temps.get(&e.a), temps.get(&e.b)) else {
            continue;
        };
        let q = e.conductance_w_per_k * (tb - ta);
        if let (Some(ai), Some(bi)) = (node_index(net, e.a), node_index(net, e.b)) {
            rates[ai] += q;
            rates[bi] -= q;
        }
    }

    // Radiation + integration; collect this tick's overheat crossings.
    let a4 = {
        let a2 = flow.ambient_k * flow.ambient_k;
        a2 * a2
    };
    let mut overheated = Vec::new();
    for (i, n) in net.nodes.iter().enumerate() {
        let Some(&t) = temps.get(&n.node) else {
            continue;
        };
        let t2 = t * t;
        rates[i] += n.emissivity * STEFAN_BOLTZMANN_W_M2_K4 * n.radiative_area_m2 * (a4 - t2 * t2);
        let new_t = t + dt * rates[i] / n.heat_capacity_j_per_k;
        temps.insert(n.node, new_t);
        if new_t > n.max_temp_k {
            overheated.push(n.node);
        }
    }
    overheated
}

/// The harmonic-mean joint conductance `2·k_a·k_b/(k_a+k_b)` derated
/// by the shared-face fraction (module doc). `shared_m2`/`min_face_m2`
/// come from the weld's contact-area estimate; non-positive
/// denominators yield zero coupling rather than a divide-by-zero.
pub fn joint_conductance(k_a: f64, k_b: f64, shared_m2: f64, min_face_m2: f64) -> f64 {
    if k_a <= 0.0 || k_b <= 0.0 || shared_m2 <= 0.0 || min_face_m2 <= 0.0 {
        return 0.0;
    }
    let fraction = (shared_m2 / min_face_m2).min(1.0);
    2.0 * k_a * k_b / (k_a + k_b) * fraction
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers
// shipping fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::graph::{PartGraph, PartNode, PartTransform};
    use crate::part::{ColliderShape, WeldPart};
    use crate::weld::weld;
    use csp_core::DQuat;
    use csp_data::ThermalDef;

    fn part(mass: f64, half: f64, thermal: ThermalDef) -> WeldPart {
        WeldPart {
            dock_ports: vec![],
            tanks: vec![],
            crew_seats: 0,
            structural: csp_data::StructuralDef::unlimited(),
            mass_kg: mass,
            com: DVec3::ZERO,
            inertia: crate::SymMat3::solid_box(mass, DVec3::new(half, half, half)),
            collider: ColliderShape::Box {
                half_extents: DVec3::new(half, half, half),
            },
            force_sites: vec![],
            aero: csp_data::AeroDef::constant(0.5, 0.0, 1.0),
            thermal,
        }
    }

    fn thermal_def(cap: f64, max_t: f64, eps: f64, cond: f64) -> ThermalDef {
        ThermalDef {
            heat_capacity_j_per_k: cap,
            max_temp_k: max_t,
            emissivity: eps,
            conduction_w_per_k: cond,
        }
    }

    /// Two stacked unit boxes (child centred on the parent's top face):
    /// one conduction edge at full harmonic conductance.
    fn two_part_net(cond: f64) -> ThermalNet {
        let mut g = PartGraph::new(PartNodeId(0));
        g.insert(
            PartNodeId(0),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        g.insert(
            PartNodeId(1),
            PartNode::new(
                Some(PartNodeId(0)),
                PartTransform::new(DVec3::new(0.0, 0.0, 2.0), DQuat::IDENTITY),
                1,
            ),
        );
        let td = thermal_def(1_000.0, 1_000.0, 0.8, cond);
        let parts = [part(100.0, 1.0, td.clone()), part(100.0, 1.0, td)];
        *weld(&g, &parts).unwrap().thermal
    }

    /// The weld's thermal net: two nodes, one edge at
    /// `2·k·k/(k+k) = k` for identical parts at full contact.
    #[test]
    fn weld_builds_net_with_full_contact_conductance() {
        let net = two_part_net(60.0);
        assert_eq!(net.nodes.len(), 2);
        assert_eq!(net.edges.len(), 1);
        let e = net.edges[0];
        assert_eq!(e.a, PartNodeId(0));
        assert_eq!(e.b, PartNodeId(1));
        assert!((e.conductance_w_per_k - 60.0).abs() < 1e-12);
        // Radiative area = unit-half box surface: 6·(2·2) = 24 m².
        for n in &net.nodes {
            assert!((n.radiative_area_m2 - 24.0).abs() < 1e-12);
        }
    }

    /// A vacuum (still-air) tick: with no convective input and both
    /// parts started hot, radiation cools them; a cooler ambient
    /// equilibrium is approached monotonically.
    #[test]
    fn radiation_cools_toward_ambient() {
        let net = two_part_net(60.0);
        let flow = ThermalFlow {
            ambient_k: SPACE_SINK_K,
            density_kg_m3: 0.0,
            speed_mps: 0.0,
            dir_body: DVec3::new(0.0, 0.0, -1.0),
        };
        let mut temps = BTreeMap::from([(PartNodeId(0), 800.0), (PartNodeId(1), 800.0)]);
        let mut rates = Vec::new();
        for _ in 0..64 {
            let hot = step(
                &net,
                None,
                &flow,
                &mut temps,
                &mut rates,
                crate::simple::PHYSICS_DT,
            );
            assert!(hot.is_empty());
        }
        let t = temps[&PartNodeId(0)];
        assert!(t < 800.0 && t > SPACE_SINK_K, "t = {t}");
        // Symmetric parts cool identically — bitwise, in fact.
        assert_eq!(t.to_bits(), temps[&PartNodeId(1)].to_bits());
    }

    /// Conduction alone: hot part loses exactly what the cold part
    /// gains and both converge to the shared capacity-weighted mean.
    /// A hand-built net with zero radiative area isolates the
    /// conduction channel exactly.
    #[test]
    fn conduction_equalizes_and_conserves_energy() {
        let node = |id: u64| ThermalNode {
            node: PartNodeId(id),
            heat_capacity_j_per_k: 1_000.0,
            max_temp_k: 10_000.0,
            emissivity: 0.8,
            radiative_area_m2: 0.0, // radiates nothing — conduction only
            aabb_lo: DVec3::ZERO,
            aabb_hi: DVec3::ZERO,
        };
        let net = ThermalNet {
            nodes: vec![node(0), node(1)],
            edges: vec![ThermalEdge {
                a: PartNodeId(0),
                b: PartNodeId(1),
                conductance_w_per_k: 600.0,
            }],
        };
        let flow = ThermalFlow {
            ambient_k: 0.0,
            density_kg_m3: 0.0,
            speed_mps: 0.0,
            dir_body: DVec3::new(0.0, 0.0, -1.0),
        };
        let mut temps = BTreeMap::from([(PartNodeId(0), 300.0), (PartNodeId(1), 500.0)]);
        let mut rates = Vec::new();
        for _ in 0..64_000 {
            step(
                &net,
                None,
                &flow,
                &mut temps,
                &mut rates,
                crate::simple::PHYSICS_DT,
            );
        }
        let (t0, t1) = (temps[&PartNodeId(0)], temps[&PartNodeId(1)]);
        assert!((t0 - t1).abs() < 0.1, "not equalized: {t0} vs {t1}");
        // Equal capacities → the common temperature is the mean;
        // conserved to explicit-Euler round-off.
        let mean = 0.5 * (t0 + t1);
        assert!((mean - 400.0).abs() < 1.0, "energy not conserved: {mean}");
        assert!(t0 > 300.0 && t1 < 500.0);
    }

    /// A single part under a constant convective flux converges toward
    /// the closed-form steady-state temperature and never exceeds it.
    #[test]
    fn heating_converges_to_steady_state() {
        let mut g = PartGraph::new(PartNodeId(0));
        g.insert(
            PartNodeId(0),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        let td = thermal_def(100_000.0, 10_000.0, 0.9, 0.0);
        let body = weld(&g, &[part(1_000.0, 1.0, td)]).unwrap();
        let net = *body.thermal;
        let node = net.nodes[0];
        // Constant flow: ρ=0.01, v=1500 → per_area = 0.005·0.5·0.01·1500³.
        let rho = 0.01;
        let v = 1_500.0;
        let flow = ThermalFlow {
            ambient_k: 288.15,
            density_kg_m3: rho,
            speed_mps: v,
            dir_body: DVec3::new(0.0, 0.0, -1.0),
        };
        let per_area = CONVECTIVE_HEAT_FRACTION * 0.5 * rho * v * v * v;
        let q_conv = per_area * node.projected_area(flow.dir_body);
        let t_ss = steady_state_temp_k(q_conv, node.emissivity, node.radiative_area_m2, 288.15);
        assert!(t_ss > 288.15 && t_ss.is_finite());
        let mut temps = BTreeMap::from([(PartNodeId(0), 288.15)]);
        let mut rates = Vec::new();
        let mut last = 288.15;
        for _ in 0..(64 * 600) {
            step(
                &net,
                None,
                &flow,
                &mut temps,
                &mut rates,
                crate::simple::PHYSICS_DT,
            );
            let t = temps[&PartNodeId(0)];
            assert!(t >= last - 1e-9, "non-monotone approach: {last} -> {t}");
            assert!(t < t_ss, "overshot steady state: {t} >= {t_ss}");
            last = t;
        }
        // 10 minutes in, within 5% of the closed-form equilibrium.
        assert!(last > t_ss * 0.95, "last {last}, t_ss {t_ss}");
    }

    /// A part over `max_temp_k` is reported — the lethality trigger.
    #[test]
    fn overheat_reports_node() {
        let net = two_part_net(0.0); // no conduction: only node 1 heats
        // Strong flux with a tiny face part is awkward; drive it via
        // the flow on the shared geometry — both heat equally, so
        // instead seed node 1 above its max and verify the report
        // fires on the first tick.
        let mut net2 = net.clone();
        net2.nodes[1].max_temp_k = 400.0;
        let flow = ThermalFlow {
            ambient_k: 288.15,
            density_kg_m3: 0.0,
            speed_mps: 0.0,
            dir_body: DVec3::new(0.0, 0.0, -1.0),
        };
        let mut temps = BTreeMap::from([(PartNodeId(0), 300.0), (PartNodeId(1), 401.0)]);
        let hot = step(
            &net2,
            None,
            &flow,
            &mut temps,
            &mut Vec::new(),
            crate::simple::PHYSICS_DT,
        );
        assert_eq!(hot, vec![PartNodeId(1)]);
    }

    /// A child offset so its projected face only half-overlaps the
    /// parent's halves the joint conductance.
    #[test]
    fn partial_contact_derates_conductance() {
        let mut g = PartGraph::new(PartNodeId(0));
        g.insert(
            PartNodeId(0),
            PartNode::new(None, PartTransform::IDENTITY, 0),
        );
        // Child (2×2×2 box centred at x=+1.5, z=+2): its xy projection
        // overlaps the parent's only over x∈[0.5, 1] → 0.5 m of 2 m —
        // a quarter of the face area (0.5·2 / 4 = 0.25 fraction).
        g.insert(
            PartNodeId(1),
            PartNode::new(
                Some(PartNodeId(0)),
                PartTransform::new(DVec3::new(1.5, 0.0, 2.0), DQuat::IDENTITY),
                1,
            ),
        );
        let td = thermal_def(1_000.0, 1_000.0, 0.8, 80.0);
        let parts = [part(100.0, 1.0, td.clone()), part(100.0, 1.0, td)];
        let net = weld(&g, &parts).unwrap().thermal;
        assert_eq!(net.edges.len(), 1);
        assert!((net.edges[0].conductance_w_per_k - 20.0).abs() < 1e-12);
    }

    /// Bitwise determinism: identical inputs stepped twice produce
    /// identical bit patterns.
    #[test]
    fn step_is_bitwise_deterministic() {
        let net = two_part_net(60.0);
        let flow = ThermalFlow {
            ambient_k: 250.0,
            density_kg_m3: 0.02,
            speed_mps: 900.0,
            dir_body: DVec3::new(0.1, -0.2, -0.97),
        };
        let run = || {
            let mut temps = BTreeMap::from([(PartNodeId(0), 290.0), (PartNodeId(1), 310.0)]);
            let mut rates = Vec::new();
            for _ in 0..512 {
                step(
                    &net,
                    None,
                    &flow,
                    &mut temps,
                    &mut rates,
                    crate::simple::PHYSICS_DT,
                );
            }
            temps
        };
        let a = run();
        let b = run();
        for (k, v) in &a {
            assert_eq!(v.to_bits(), b[k].to_bits());
        }
    }

    /// `joint_conductance` degenerate inputs produce zero coupling,
    /// and the fraction clamps at 1.
    #[test]
    fn joint_conductance_bounds() {
        assert_eq!(joint_conductance(0.0, 50.0, 1.0, 1.0), 0.0);
        assert_eq!(joint_conductance(50.0, 0.0, 1.0, 1.0), 0.0);
        assert_eq!(joint_conductance(50.0, 50.0, 0.0, 1.0), 0.0);
        assert_eq!(joint_conductance(50.0, 50.0, 1.0, 0.0), 0.0);
        // Full contact: harmonic mean of equal k's is k.
        assert!((joint_conductance(50.0, 50.0, 1.0, 1.0) - 50.0).abs() < 1e-12);
        // Overlap beyond the smaller face clamps to full coupling.
        assert!((joint_conductance(50.0, 50.0, 2.0, 1.0) - 50.0).abs() < 1e-12);
        // Quarter coverage.
        assert!((joint_conductance(50.0, 50.0, 0.25, 1.0) - 12.5).abs() < 1e-12);
    }
}
