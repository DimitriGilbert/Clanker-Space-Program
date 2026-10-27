//! The §3.1/§3.4 `data/parts/*.ron` rules: id↔stem, unique IDs,
//! `schema_version`, positive masses/sizes, variant consistency,
//! `ScaleRule` sanity, attach-node well-formedness, category-
//! consistent modules, resource references, and the §3.4 reliability
//! ranges.

use std::collections::BTreeSet;

use crate::ids::LocKey;

use crate::load::{LoadedPart, LoadedParts};
use crate::loc::LocFile;
use crate::part::{ColliderDef, ModuleDef, PART_SCHEMA_VERSION, PartCategory, PartDef, ScaleRule};
use crate::resource::ResourceFile;

use super::ValidationError;
use super::common::{check_id, check_situation_mask, part_positive};
// ---------- part defs + resource registry (§3.1, §3.4) ----------

/// Upper bound for [`crate::part::ScaleExponents`] — see the type's
/// doc: exponents beyond 4 are authoring errors, not supported physics.
const SCALE_EXPONENT_MAX: f64 = 4.0;

/// `‖direction‖ = 1` tolerance for attach-node normals.
const NODE_DIR_UNIT_TOL: f64 = 1e-6;

/// Relative/absolute tolerance on the engine propellant-fraction sum
/// (fractions are authored and must total exactly 1 — within float
/// authoring noise).
const PROPELLANT_SUM_TOL: f64 = 1e-6;

/// `(resource, amount)` list checks shared by `build_resources`,
/// `Tank.capacity`, and `Engine.propellants`: IDs unique, known to
/// `resources.ron`, amounts finite and positive.
fn check_resource_list(
    part: &str,
    context: &'static str,
    list: &[(crate::ids::ResourceId, f64)],
    res: &ResourceFile,
    out: &mut Vec<ValidationError>,
) {
    let mut seen = BTreeSet::new();
    for (id, amount) in list {
        if !seen.insert(id.as_str()) {
            out.push(ValidationError::DuplicateResource {
                context: format!("{part} {context}"),
                resource: id.as_str().to_string(),
            });
        }
        if !res.contains(id) {
            out.push(ValidationError::UnknownResource {
                part: part.to_string(),
                resource: id.as_str().to_string(),
            });
        }
        part_positive(part, context, *amount, out);
    }
}

fn check_vec3_finite(
    part: &str,
    field: &'static str,
    v: &csp_core::DVec3,
    out: &mut Vec<ValidationError>,
) {
    if !(v.x.is_finite() && v.y.is_finite() && v.z.is_finite()) {
        out.push(ValidationError::PartNonFinite {
            part: part.to_string(),
            field,
        });
    }
}

fn check_scale_rule(part: &str, scale: &ScaleRule, out: &mut Vec<ValidationError>) {
    match scale {
        ScaleRule::None => {}
        ScaleRule::Discrete(factors) => {
            if factors.is_empty() {
                out.push(ValidationError::BadScaleRule {
                    part: part.to_string(),
                    reason: "discrete list is empty",
                });
                return;
            }
            let mut prev = 0.0;
            for f in factors {
                if !f.is_finite() || *f <= 0.0 {
                    out.push(ValidationError::BadScaleRule {
                        part: part.to_string(),
                        reason: "discrete factor non-positive or non-finite",
                    });
                    return;
                }
                if *f <= prev {
                    out.push(ValidationError::BadScaleRule {
                        part: part.to_string(),
                        reason: "discrete factors not strictly increasing",
                    });
                    return;
                }
                prev = *f;
            }
            if !factors.contains(&1.0) {
                out.push(ValidationError::BadScaleRule {
                    part: part.to_string(),
                    reason: "discrete list lacks the 1.0 baseline",
                });
            }
        }
        ScaleRule::Continuous {
            min,
            max,
            exponents,
        } => {
            if !(min.is_finite() && max.is_finite()) {
                out.push(ValidationError::BadScaleRule {
                    part: part.to_string(),
                    reason: "range bound non-finite",
                });
            } else if !(*min > 0.0 && *min <= 1.0 && *max >= 1.0) {
                out.push(ValidationError::BadScaleRule {
                    part: part.to_string(),
                    reason: "range must satisfy 0 < min <= 1 <= max",
                });
            }
            for (name, e) in [
                ("mass", exponents.mass),
                ("cost", exponents.cost),
                ("thrust", exponents.thrust),
                ("capacity", exponents.capacity),
                ("structural", exponents.structural),
            ] {
                if !(e.is_finite() && (0.0..=SCALE_EXPONENT_MAX).contains(&e)) {
                    out.push(ValidationError::BadScaleRule {
                        part: part.to_string(),
                        reason: match name {
                            "mass" => "mass exponent outside [0, 4]",
                            "cost" => "cost exponent outside [0, 4]",
                            "thrust" => "thrust exponent outside [0, 4]",
                            "capacity" => "capacity exponent outside [0, 4]",
                            _ => "structural exponent outside [0, 4]",
                        },
                    });
                }
            }
        }
    }
}

fn check_attach_nodes(part: &PartDef, out: &mut Vec<ValidationError>) {
    let part_id = part.id.as_str();
    if part.attach_nodes.is_empty() {
        out.push(ValidationError::BadAttachNode {
            part: part_id.to_string(),
            node: "(part)".to_string(),
            reason: "part declares no attach nodes",
        });
    }
    let mut seen = BTreeSet::new();
    for (i, n) in part.attach_nodes.iter().enumerate() {
        let node = if n.id.is_empty() {
            format!("#{i}")
        } else {
            n.id.clone()
        };
        if n.id.is_empty() {
            out.push(ValidationError::BadAttachNode {
                part: part_id.to_string(),
                node: node.clone(),
                reason: "empty node id",
            });
        } else if !seen.insert(n.id.as_str()) {
            out.push(ValidationError::BadAttachNode {
                part: part_id.to_string(),
                node: node.clone(),
                reason: "duplicate node id",
            });
        }
        check_vec3_finite(part_id, "attach_nodes.position_m", &n.position_m, out);
        let len = libm::sqrt(
            n.direction.x * n.direction.x
                + n.direction.y * n.direction.y
                + n.direction.z * n.direction.z,
        );
        if !(len.is_finite() && (len - 1.0).abs() <= NODE_DIR_UNIT_TOL) {
            out.push(ValidationError::BadAttachNode {
                part: part_id.to_string(),
                node,
                reason: "direction is not unit length",
            });
        }
        part_positive(part_id, "attach_nodes.diameter_m", n.diameter_m, out);
    }
}

fn check_collider(part: &str, collider: &ColliderDef, out: &mut Vec<ValidationError>) {
    match collider {
        ColliderDef::Box { half_extents_m } => {
            for v in [half_extents_m.x, half_extents_m.y, half_extents_m.z] {
                part_positive(part, "collider.half_extents_m", v, out);
            }
        }
        ColliderDef::Cylinder {
            radius_m,
            half_height_m,
        } => {
            part_positive(part, "collider.radius_m", *radius_m, out);
            part_positive(part, "collider.half_height_m", *half_height_m, out);
        }
        ColliderDef::Convex { points } => {
            if points.len() < 4 {
                out.push(ValidationError::BadCollider {
                    part: part.to_string(),
                    reason: "convex hull needs at least 4 points",
                });
            }
            for p in points {
                check_vec3_finite(part, "collider.points", p, out);
            }
        }
    }
}

fn check_aero(part: &PartDef, out: &mut Vec<ValidationError>) {
    let part_id = part.id.as_str();
    for curve in [&part.aero.cd_curve, &part.aero.cl_curve] {
        if curve.is_empty() {
            out.push(ValidationError::BadAeroCurve {
                part: part_id.to_string(),
                reason: "curve is empty",
            });
            continue;
        }
        let mut prev = f64::NEG_INFINITY;
        for k in curve {
            if !k.mach.is_finite() || !k.coeff.is_finite() {
                out.push(ValidationError::BadAeroCurve {
                    part: part_id.to_string(),
                    reason: "non-finite knot",
                });
                break;
            }
            if k.mach < 0.0 {
                out.push(ValidationError::BadAeroCurve {
                    part: part_id.to_string(),
                    reason: "negative mach",
                });
                break;
            }
            if k.mach <= prev {
                out.push(ValidationError::BadAeroCurve {
                    part: part_id.to_string(),
                    reason: "mach not strictly increasing",
                });
                break;
            }
            if k.coeff < 0.0 {
                out.push(ValidationError::BadAeroCurve {
                    part: part_id.to_string(),
                    reason: "negative coefficient",
                });
                break;
            }
            prev = k.mach;
        }
    }
    part_positive(part_id, "aero.ref_len_m", part.aero.ref_len_m, out);
}

fn check_variants(part: &PartDef, out: &mut Vec<ValidationError>) {
    let part_id = part.id.as_str();
    if part.variants.is_empty() {
        out.push(ValidationError::EmptyVariants {
            part: part_id.to_string(),
        });
    }
    let mut seen = BTreeSet::new();
    for v in &part.variants {
        check_id(v.id.as_str(), out);
        if !seen.insert(v.id.as_str()) {
            out.push(ValidationError::DuplicateVariantId {
                part: part_id.to_string(),
                variant: v.id.as_str().to_string(),
            });
        }
        if !(v.cost_multiplier.is_finite() && v.cost_multiplier > 0.0) {
            out.push(ValidationError::BadVariantMultiplier {
                part: part_id.to_string(),
                variant: v.id.as_str().to_string(),
                stat: "cost_multiplier",
                value: v.cost_multiplier,
            });
        }
        let mut stats = BTreeSet::new();
        for (stat, m) in &v.stat_multipliers {
            if !stats.insert(*stat) {
                out.push(ValidationError::DuplicateVariantStat {
                    part: part_id.to_string(),
                    variant: v.id.as_str().to_string(),
                    stat: stat.name(),
                });
            }
            if !(m.is_finite() && *m > 0.0) {
                out.push(ValidationError::BadVariantMultiplier {
                    part: part_id.to_string(),
                    variant: v.id.as_str().to_string(),
                    stat: stat.name(),
                    value: *m,
                });
            }
            // `csp_phys::resolve`'s `UnresolvableStatPath` gate, mirrored
            // with its own predicate (`StatPath::base_value` — the same
            // call `check_tree_tracks` runs for upgrade tracks): a
            // multiplier on a stat the def lacks loads green today and
            // only fails at manufacture.
            if stat.base_value(part).is_none() {
                out.push(ValidationError::UnresolvableVariantStat {
                    part: part_id.to_string(),
                    variant: v.id.as_str().to_string(),
                    stat: stat.name(),
                });
            }
        }
    }
}

/// Per-module field checks — dispatch is in [`check_modules`]; the
/// engine's checks live in [`check_engine_module`]. `recipe_ids` and
/// `experiment_ids` resolve the `Converter`/`Experiment` cross-
/// references — the data set the modules point at, not the module's
/// own fields.
fn check_module(
    part_id: &str,
    res: &ResourceFile,
    recipe_ids: &BTreeSet<&str>,
    experiment_ids: &BTreeSet<&str>,
    m: &ModuleDef,
    out: &mut Vec<ValidationError>,
) {
    match m {
        ModuleDef::Engine { .. } => check_engine_module(part_id, res, m, out),
        ModuleDef::Tank { capacity } => {
            if capacity.is_empty() {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "tank declares no capacities",
                });
            } else {
                check_resource_list(part_id, "tank.capacity", capacity, res, out);
            }
        }
        ModuleDef::Control { torque_nm, power_w } => {
            if !(torque_nm.is_finite() && *torque_nm >= 0.0) {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "control torque_nm negative or non-finite",
                });
            }
            if !(power_w.is_finite() && *power_w >= 0.0) {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "control power_w negative or non-finite",
                });
            }
        }
        ModuleDef::Decoupler => {}
        ModuleDef::DockPort { .. } => {}
        ModuleDef::CrewSeat { slots } => {
            if *slots == 0 {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "crew seat with zero slots",
                });
            }
        }
        ModuleDef::Experiment {
            kind,
            data_units,
            situations,
            ..
        } => {
            check_id(kind.as_str(), out);
            if !experiment_ids.contains(kind.as_str()) {
                out.push(ValidationError::UnknownExperimentKind {
                    part: part_id.to_string(),
                    experiment: kind.as_str().to_string(),
                });
            }
            part_positive(part_id, "experiment.data_units", *data_units, out);
            check_situation_mask(
                *situations,
                "experiment can run in no situation",
                |reason| {
                    out.push(ValidationError::BadModule {
                        part: part_id.to_string(),
                        reason,
                    });
                },
            );
        }
        ModuleDef::Drill { .. } => check_drill_module(part_id, res, m, out),
        ModuleDef::Converter {
            recipe, power_w, ..
        } => {
            check_id(recipe.as_str(), out);
            if !recipe_ids.contains(recipe.as_str()) {
                out.push(ValidationError::UnknownRecipe {
                    part: part_id.to_string(),
                    recipe: recipe.as_str().to_string(),
                });
            }
            if !(power_w.is_finite() && *power_w >= 0.0) {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "converter power_w negative or non-finite",
                });
            }
        }
        ModuleDef::Lab {
            throughput_data_per_s,
            multiplier,
            researcher_slots,
        } => {
            part_positive(
                part_id,
                "lab.throughput_data_per_s",
                *throughput_data_per_s,
                out,
            );
            part_positive(part_id, "lab.multiplier", *multiplier, out);
            if *researcher_slots == 0 {
                out.push(ValidationError::BadModule {
                    part: part_id.to_string(),
                    reason: "lab with zero researcher slots",
                });
            }
        }
        ModuleDef::Parachute { .. } => check_parachute_module(part_id, m, out),
    }
}

/// The `Parachute` module's own rules: positive catch ceilings,
/// non-negative areas with deployed > packed (an equal-or-smaller
/// canopy is a no-op deploy, not a decelerator), and a non-negative
/// inflation ramp.
fn check_parachute_module(part_id: &str, m: &ModuleDef, out: &mut Vec<ValidationError>) {
    let ModuleDef::Parachute {
        deploy_alt_m,
        deploy_speed_mps,
        packed_drag_area_m2,
        deployed_drag_area_m2,
        deploy_time_s,
    } = m
    else {
        return;
    };
    part_positive(part_id, "parachute.deploy_alt_m", *deploy_alt_m, out);
    part_positive(
        part_id,
        "parachute.deploy_speed_mps",
        *deploy_speed_mps,
        out,
    );
    if !(packed_drag_area_m2.is_finite() && *packed_drag_area_m2 >= 0.0) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "parachute packed_drag_area_m2 negative or non-finite",
        });
    }
    if !(deployed_drag_area_m2.is_finite()
        && packed_drag_area_m2.is_finite()
        && *deployed_drag_area_m2 > *packed_drag_area_m2)
    {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "parachute deployed_drag_area_m2 must exceed packed_drag_area_m2",
        });
    }
    if !(deploy_time_s.is_finite() && *deploy_time_s >= 0.0) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "parachute deploy_time_s negative or non-finite",
        });
    }
}

/// The `Drill` module's rules: a known resource, positive rate,
/// `min_concentration ∈ [0, 1]`, non-negative power draw.
fn check_drill_module(
    part_id: &str,
    res: &ResourceFile,
    m: &ModuleDef,
    out: &mut Vec<ValidationError>,
) {
    let ModuleDef::Drill {
        resource,
        base_rate_per_s,
        min_concentration,
        power_w,
    } = m
    else {
        return;
    };
    if !res.contains(resource) {
        out.push(ValidationError::UnknownResource {
            part: part_id.to_string(),
            resource: resource.as_str().to_string(),
        });
    }
    part_positive(part_id, "drill.base_rate_per_s", *base_rate_per_s, out);
    if !(min_concentration.is_finite() && (0.0..=1.0).contains(min_concentration)) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "drill min_concentration outside [0, 1]",
        });
    }
    if !(power_w.is_finite() && *power_w >= 0.0) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "drill power_w negative or non-finite",
        });
    }
}

/// The `Engine` module's own rules: positive thrust/Isp, the
/// `isp_sl ≤ isp_vac` ordering, a sane throttle band, non-degenerate
/// gimbal/ignition/rated-burn fields, and propellant fractions that
/// are unique, known, positive, and total 1.
fn check_engine_module(
    part_id: &str,
    res: &ResourceFile,
    m: &ModuleDef,
    out: &mut Vec<ValidationError>,
) {
    let ModuleDef::Engine {
        thrust_vac_n,
        isp_vac_s,
        isp_sl_s,
        throttle_range,
        gimbal_deg,
        ignitions,
        rated_burn_s,
        propellants,
        ullage_required: _,
    } = m
    else {
        return;
    };
    part_positive(part_id, "engine.thrust_vac_n", *thrust_vac_n, out);
    part_positive(part_id, "engine.isp_vac_s", *isp_vac_s, out);
    part_positive(part_id, "engine.isp_sl_s", *isp_sl_s, out);
    if isp_sl_s.is_finite() && isp_vac_s.is_finite() && isp_sl_s > isp_vac_s {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "isp_sl_s exceeds isp_vac_s",
        });
    }
    if !(throttle_range.min.is_finite()
        && throttle_range.max.is_finite()
        && throttle_range.min >= 0.0
        && throttle_range.min < throttle_range.max
        && throttle_range.max <= 1.0)
    {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "throttle_range outside 0 <= min < max <= 1",
        });
    }
    if !(gimbal_deg.is_finite() && *gimbal_deg >= 0.0) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "gimbal_deg negative or non-finite",
        });
    }
    if let Some(n) = ignitions
        && *n == 0
    {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "ignitions is Some(0) — use None for unlimited",
        });
    }
    part_positive(part_id, "engine.rated_burn_s", *rated_burn_s, out);
    if propellants.is_empty() {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "engine declares no propellants",
        });
    } else {
        check_resource_list(part_id, "engine.propellants", propellants, res, out);
        let sum: f64 = propellants.iter().map(|(_, f)| f).sum();
        // `csp_phys::resolve::check_engine_propellants`' gate, mirrored
        // with the same polarity: a non-finite sum (two authored
        // fractions can overflow to +inf at ~1e308 apiece) is a
        // rejection, not a pass — the resolver errors on it, so the
        // validator must too.
        if !(sum.is_finite() && (sum - 1.0).abs() <= PROPELLANT_SUM_TOL) {
            out.push(ValidationError::BadModule {
                part: part_id.to_string(),
                reason: "propellant mass fractions do not sum to 1",
            });
        }
    }
}

fn check_modules(
    part: &PartDef,
    res: &ResourceFile,
    recipe_ids: &BTreeSet<&str>,
    experiment_ids: &BTreeSet<&str>,
    out: &mut Vec<ValidationError>,
) {
    let part_id = part.id.as_str();
    for m in &part.modules {
        check_module(part_id, res, recipe_ids, experiment_ids, m, out);
    }
    let has = |pred: fn(&ModuleDef) -> bool| part.modules.iter().any(pred);
    let has_engine = has(|m| matches!(m, ModuleDef::Engine { .. }));
    let has_tank = has(|m| matches!(m, ModuleDef::Tank { .. }));
    let has_crew_or_control =
        has(|m| matches!(m, ModuleDef::CrewSeat { .. } | ModuleDef::Control { .. }));
    let has_tank_or_dock =
        has(|m| matches!(m, ModuleDef::Tank { .. } | ModuleDef::DockPort { .. }));
    let has_experiment = has(|m| matches!(m, ModuleDef::Experiment { .. }));
    if part
        .modules
        .iter()
        .filter(|m| matches!(m, ModuleDef::Engine { .. }))
        .count()
        > 1
    {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "more than one engine module",
        });
    }
    // One decelerator per part — the sim's per-node deploy record is
    // a single flag; a second spec would be ambiguous authored data.
    if part
        .modules
        .iter()
        .filter(|m| matches!(m, ModuleDef::Parachute { .. }))
        .count()
        > 1
    {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "more than one parachute module",
        });
    }
    // `ScaleRule::Discrete` carries no exponents, so the resolver
    // scales a discrete part's geometry only — a `Tank` or `Engine`
    // module on one would advertise sizes whose stats silently do not
    // scale. Forbidden outright (parts that need stat scaling author
    // `Continuous`); passive modules stay legal.
    if matches!(part.scale, ScaleRule::Discrete(_)) && (has_engine || has_tank) {
        out.push(ValidationError::BadModule {
            part: part_id.to_string(),
            reason: "discrete scaling carries no exponents — tank/engine stats would not scale",
        });
    }
    // M6-B: a `DockPort`'s `node` must name one of the part's attach
    // nodes — it is the docking face the sim anchors soft-capture at —
    // and two port modules may not claim the same node (one face, one
    // capture point; a second face authors a second node).
    let mut port_nodes = BTreeSet::new();
    for m in &part.modules {
        if let ModuleDef::DockPort { node, .. } = m {
            if !part.attach_nodes.iter().any(|n| &n.id == node) {
                out.push(ValidationError::BadModuleNode {
                    part: part_id.to_string(),
                    node: node.clone(),
                    reason: "dock_port node names no attach node",
                });
            } else if !port_nodes.insert(node.as_str()) {
                out.push(ValidationError::BadModuleNode {
                    part: part_id.to_string(),
                    node: node.clone(),
                    reason: "two dock ports claim the same attach node",
                });
            }
        }
    }
    // §3.1 category-consistent stats: the category names the module the
    // part must carry (mapping documented on PartCategory).
    let required: Option<&'static str> = match part.category {
        PartCategory::Engine if !has_engine => Some("engine"),
        PartCategory::FuelTank if !has_tank => Some("tank"),
        PartCategory::Control if !has_crew_or_control => Some("crew_seat or control"),
        PartCategory::Payload if !has_tank_or_dock => Some("tank or dock_port"),
        PartCategory::Science if !has_experiment => Some("experiment"),
        _ => None,
    };
    if let Some(required) = required {
        out.push(ValidationError::CategoryModuleMismatch {
            part: part_id.to_string(),
            category: part.category.name(),
            required,
        });
    }
}

fn check_reliability(part: &PartDef, out: &mut Vec<ValidationError>) {
    let part_id = part.id.as_str();
    let r = &part.reliability;
    part_positive(part_id, "reliability.base_mtbf_s", r.base_mtbf_s, out);
    if !(r.infant_mortality.is_finite() && (0.0..1.0).contains(&r.infant_mortality)) {
        out.push(ValidationError::BadReliability {
            part: part_id.to_string(),
            reason: "infant_mortality outside [0, 1)",
        });
    }
    if !(r.dormant_stress.is_finite() && (0.0..=1.0).contains(&r.dormant_stress)) {
        out.push(ValidationError::BadReliability {
            part: part_id.to_string(),
            reason: "dormant_stress outside [0, 1]",
        });
    }
    let knots = &r.qualification_curve.knots;
    if knots.is_empty() {
        out.push(ValidationError::BadReliability {
            part: part_id.to_string(),
            reason: "qualification curve is empty",
        });
    }
    let mut prev = f64::NEG_INFINITY;
    for k in knots {
        if !k.qualification_units.is_finite() || !k.hazard_multiplier.is_finite() {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "non-finite qualification knot",
            });
            break;
        }
        if k.qualification_units < 0.0 || k.qualification_units <= prev {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "qualification units negative or not strictly increasing",
            });
            break;
        }
        if !(0.0..=1.0).contains(&k.hazard_multiplier) || k.hazard_multiplier <= 0.0 {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "hazard multiplier outside (0, 1]",
            });
            break;
        }
        prev = k.qualification_units;
    }
    for s in &r.regime_stress {
        if !(s.threshold.is_finite() && s.threshold >= 0.0) {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "stress threshold negative or non-finite",
            });
        }
        if !(s.multiplier.is_finite() && s.multiplier >= 1.0) {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "stress multiplier below 1 — stress raises hazard",
            });
        }
    }
    if r.failure_modes.is_empty() {
        out.push(ValidationError::BadReliability {
            part: part_id.to_string(),
            reason: "no failure modes",
        });
    }
    for (_, w) in &r.failure_modes {
        if !(w.is_finite() && *w > 0.0) {
            out.push(ValidationError::BadReliability {
                part: part_id.to_string(),
                reason: "failure-mode weight not positive",
            });
        }
    }
}

/// §3.1 part rules over the whole `data/parts/` set: per-def field and
/// module checks plus set-level ID uniqueness and id↔filename
/// agreement. `res` resolves every `ResourceId` a part references
/// (`build_resources`, tank capacities, propellants, drill outputs);
/// `recipes` and `experiments` resolve the `Converter::recipe` and
/// `Experiment::kind` cross-references — a converter naming a missing
/// recipe is a dead module ([`ValidationError::UnknownRecipe`]), an
/// instrument naming a missing experiment kind can never run
/// ([`ValidationError::UnknownExperimentKind`]).
pub fn validate_parts(
    loaded: &LoadedParts,
    res: &ResourceFile,
    recipes: &crate::load::LoadedRecipes,
    experiments: &crate::load::LoadedExperiments,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let recipe_ids: BTreeSet<&str> = recipes.recipes.iter().map(|r| r.def.id.as_str()).collect();
    let experiment_ids: BTreeSet<&str> = experiments
        .experiments
        .iter()
        .map(|e| e.def.id.as_str())
        .collect();
    let mut ids = BTreeSet::new();
    for p in &loaded.parts {
        check_part(p, res, &recipe_ids, &experiment_ids, &mut out);
        let id = p.def.id.as_str();
        check_id(id, &mut out);
        if !ids.insert(id) {
            out.push(ValidationError::DuplicatePartId { id: id.to_string() });
        }
        let stem = p
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if stem != id {
            out.push(ValidationError::PartIdFileMismatch {
                id: id.to_string(),
                file: p.path.display().to_string(),
            });
        }
    }
    out
}

/// All per-def checks for one loaded part.
fn check_part(
    p: &LoadedPart,
    res: &ResourceFile,
    recipe_ids: &BTreeSet<&str>,
    experiment_ids: &BTreeSet<&str>,
    out: &mut Vec<ValidationError>,
) {
    let part = &p.def;
    let part_id = part.id.as_str();
    if part.schema_version != PART_SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: format!("parts/{}", p.path.display()),
            found: part.schema_version,
            expected: PART_SCHEMA_VERSION,
        });
    }
    check_id(part.family.as_str(), out);
    check_id(part.tech_requirement.as_str(), out);
    part_positive(part_id, "mass_dry_kg", part.mass_dry_kg, out);
    part_positive(part_id, "cost_funds", part.cost_funds, out);
    for v in [part.size_m.x, part.size_m.y, part.size_m.z] {
        part_positive(part_id, "size_m", v, out);
    }
    check_resource_list(part_id, "build_resources", &part.build_resources, res, out);
    check_attach_nodes(part, out);
    check_scale_rule(part_id, &part.scale, out);
    check_collider(part_id, &part.collider, out);
    check_aero(part, out);
    part_positive(
        part_id,
        "thermal.heat_capacity_j_per_k",
        part.thermal.heat_capacity_j_per_k,
        out,
    );
    part_positive(part_id, "thermal.max_temp_k", part.thermal.max_temp_k, out);
    if !(part.thermal.emissivity.is_finite()
        && part.thermal.emissivity > 0.0
        && part.thermal.emissivity <= 1.0)
    {
        out.push(ValidationError::PartNonPositive {
            part: part_id.to_string(),
            field: "thermal.emissivity",
            value: part.thermal.emissivity,
        });
    }
    if !(part.thermal.conduction_w_per_k.is_finite() && part.thermal.conduction_w_per_k >= 0.0) {
        out.push(ValidationError::PartNonPositive {
            part: part_id.to_string(),
            field: "thermal.conduction_w_per_k",
            value: part.thermal.conduction_w_per_k,
        });
    }
    // §3.4's explicit-Euler stability bound: a joint's conductance is
    // the harmonic combination of the two parts' `conduction_w_per_k`
    // (≤ 2·min(kᵢ, kⱼ)), so `k ≤ C` on every part keeps every edge's
    // dt·G·Σ(1/C) at ~0.03 or below — authored data cannot build an
    // unstable net. Gated on the scalar checks passing so one bad
    // field reports exactly one error.
    if part.thermal.heat_capacity_j_per_k.is_finite()
        && part.thermal.heat_capacity_j_per_k > 0.0
        && part.thermal.conduction_w_per_k > part.thermal.heat_capacity_j_per_k
    {
        out.push(ValidationError::ConductionExceedsCapacity {
            part: part_id.to_string(),
            conduction_w_per_k: part.thermal.conduction_w_per_k,
            heat_capacity_j_per_k: part.thermal.heat_capacity_j_per_k,
        });
    }
    part_positive(
        part_id,
        "structural.max_axial_n",
        part.structural.max_axial_n,
        out,
    );
    part_positive(
        part_id,
        "structural.max_shear_n",
        part.structural.max_shear_n,
        out,
    );
    part_positive(
        part_id,
        "structural.max_torque_nm",
        part.structural.max_torque_nm,
        out,
    );
    check_variants(part, out);
    check_modules(part, res, recipe_ids, experiment_ids, out);
    check_reliability(part, out);
}

/// Loc resolution for the part set (§8.2 "ID stability" row): part
/// display names, variant names, and `resources.ron` display names
/// must all resolve in `en.ron`. Separate from [`validate_parts`]
/// because the loc file lives outside the parts directory.
pub fn validate_part_loc_keys(
    loaded: &LoadedParts,
    res: &ResourceFile,
    loc: &LocFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    let mut check = |context: String, key: &LocKey| {
        if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
            out.push(ValidationError::UnresolvedLocKey {
                context,
                key: key.as_str().to_string(),
            });
        }
    };
    for p in &loaded.parts {
        check(format!("parts/{}", p.def.id), &p.def.display_name);
        for v in &p.def.variants {
            check(format!("parts/{} variant", p.def.id), &v.display_name);
        }
    }
    for r in &res.resources {
        check("resources.ron".to_string(), &r.display_name);
    }
    out
}
