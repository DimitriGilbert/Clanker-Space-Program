//! The §3.0 system-directory rules over a [`LoadedSystem`]:
//! manifest↔def agreement, schema versions, the parent graph (acyclic,
//! star-rooted), orbit/SOI/period geometry, atmosphere tables, biome
//! maps — and [`validate_loc_keys`], the system's [`LocKey`] sweep.

use std::collections::{BTreeMap, BTreeSet};

use crate::biome::{BIOME_MAP_MAX_H, BIOME_MAP_MAX_W, decode_indexed_png, inspect_indexed_png};

use crate::ids::LocKey;
use crate::load::LoadedSystem;
use crate::loc::LocFile;
use crate::system::{BODY_SCHEMA_VERSION, SCHEMA_VERSION};

use super::ValidationError;
use super::common::{check_finite, check_id, check_positive};
/// Relative tolerance for the §3.0 period-consistency rule:
/// `|T_authored − 2π√(a³/μ_parent)| / T_computed ≤ PERIOD_REL_TOL`.
pub const PERIOD_REL_TOL: f64 = 1e-9;

/// Body defs keyed by ID — built once by [`build_def_map`] and shared by
/// the graph and orbital rules. `BTreeMap` keeps iteration deterministic.
type DefMap<'a> = BTreeMap<&'a str, &'a crate::load::LoadedBody>;

/// The §3.0 validator over a fully loaded system. Pure: every input is
/// already in memory, so results never depend on the filesystem, the
/// clock, or iteration order of a hash container.
pub fn validate_system(sys: &LoadedSystem) -> Vec<ValidationError> {
    let mut out = Vec::new();
    check_schema_versions(sys, &mut out);
    let defs = build_def_map(sys, &mut out);
    check_body_set(sys, &defs, &mut out);
    check_graph(sys, &defs, &mut out);
    for b in &sys.bodies {
        check_body_fields(b, &mut out);
        check_orbit_rules(b, &defs, &mut out);
        if b.def.atmosphere.is_some() {
            check_atmosphere(b, &mut out);
        }
        if let Some(bm) = &b.biomes {
            check_biomes(b, bm, &mut out);
        }
    }
    // C31-F3: whatever the loader found under the system directory
    // that the manifest never referenced — each orphan is dead content
    // the data-set hash sees but nothing loads, so it is authored
    // noise (or a forgotten manifest entry) and must not validate
    // green. Recorded by `load_system_from`; hand-rolled systems carry
    // an empty list.
    for path in &sys.orphans {
        out.push(ValidationError::OrphanContent {
            path: path.display().to_string(),
        });
    }
    out
}

/// §8.2's "schema_version present" row — presence is enforced by serde at
/// parse time; this re-checks the *value* on every loaded file.
fn check_schema_versions(sys: &LoadedSystem, out: &mut Vec<ValidationError>) {
    if sys.system.schema_version != SCHEMA_VERSION {
        out.push(ValidationError::SchemaVersion {
            file: "system.ron".to_string(),
            found: sys.system.schema_version,
            expected: SCHEMA_VERSION,
        });
    }
    for b in &sys.bodies {
        if b.def.schema_version != BODY_SCHEMA_VERSION {
            out.push(ValidationError::SchemaVersion {
                file: format!("{}/body.ron", b.def.id),
                found: b.def.schema_version,
                expected: BODY_SCHEMA_VERSION,
            });
        }
        if let Some(bm) = &b.biomes
            && bm.legend.schema_version != crate::biome::BIOME_SCHEMA_VERSION
        {
            out.push(ValidationError::SchemaVersion {
                file: format!(
                    "{}/{}",
                    b.def.id,
                    b.def
                        .biome_map
                        .as_ref()
                        .map_or("biomes.ron", |r| r.legend.as_str())
                ),
                found: bm.legend.schema_version,
                expected: crate::biome::BIOME_SCHEMA_VERSION,
            });
        }
    }
}

/// Index loaded defs by ID, reporting duplicates. First def wins —
/// the convention the loaders' `by_id` views document (`or_insert`,
/// not `insert`: `insert` would make the LAST duplicate the resolved
/// def while the sim sees the first).
fn build_def_map<'a>(sys: &'a LoadedSystem, out: &mut Vec<ValidationError>) -> DefMap<'a> {
    let mut defs = DefMap::new();
    for b in &sys.bodies {
        let id = b.def.id.as_str();
        if let std::collections::btree_map::Entry::Vacant(v) = defs.entry(id) {
            v.insert(b);
        } else {
            out.push(ValidationError::DuplicateBodyId { id: id.to_string() });
        }
    }
    defs
}

/// Manifest ↔ loaded-set agreement, plus ID hygiene on both sides.
fn check_body_set(sys: &LoadedSystem, defs: &DefMap, out: &mut Vec<ValidationError>) {
    check_id(sys.system.id.as_str(), out);
    check_id(sys.system.star.as_str(), out);
    let mut listed: BTreeSet<&str> = BTreeSet::new();
    for id in &sys.system.bodies {
        listed.insert(id.as_str());
        check_id(id.as_str(), out);
        if !defs.contains_key(id.as_str()) {
            out.push(ValidationError::ListedIdNotDeclared {
                id: id.as_str().to_string(),
            });
        }
    }
    for &id in defs.keys() {
        check_id(id, out);
        if !listed.contains(id) {
            out.push(ValidationError::DeclaredIdNotListed { id: id.to_string() });
        }
    }
}

/// Star shape, parent-exists / orbit-shape checks, acyclicity, and
/// reachability of every body from `system.star`.
fn check_graph(sys: &LoadedSystem, defs: &DefMap, out: &mut Vec<ValidationError>) {
    let star_id = sys.system.star.as_str();
    let star_known = if let Some(star) = defs.get(star_id) {
        if star.def.parent.is_some() {
            out.push(ValidationError::StarHasParent {
                id: star_id.to_string(),
            });
        }
        if star.def.orbit.is_some() {
            out.push(ValidationError::OrbitOnRoot {
                body: star_id.to_string(),
            });
        }
        true
    } else {
        out.push(ValidationError::StarNotFound {
            id: star_id.to_string(),
        });
        false
    };
    for b in &sys.bodies {
        let id = b.def.id.as_str();
        match &b.def.parent {
            None => {
                if id != star_id {
                    out.push(ValidationError::RootNotStar { id: id.to_string() });
                }
            }
            Some(p) => {
                if !defs.contains_key(p.as_str()) {
                    out.push(ValidationError::ParentNotFound {
                        body: id.to_string(),
                        parent: p.as_str().to_string(),
                    });
                }
                if b.def.orbit.is_none() {
                    out.push(ValidationError::OrbitMissing {
                        body: id.to_string(),
                    });
                }
            }
        }
    }
    if star_known {
        check_parent_chains(sys, defs, star_id, out);
    }
}

/// Walk every parent chain: a revisit is a cycle; a resolved chain that
/// terminates anywhere but the star is unreachable (§8.2 "reachability").
fn check_parent_chains(
    sys: &LoadedSystem,
    defs: &DefMap,
    star_id: &str,
    out: &mut Vec<ValidationError>,
) {
    for b in &sys.bodies {
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut cur = b.def.id.as_str();
        seen.insert(cur);
        let mut parent = b.def.parent.as_ref();
        while let Some(p) = parent {
            let pid = p.as_str();
            if !seen.insert(pid) {
                out.push(ValidationError::ParentCycle {
                    body: b.def.id.as_str().to_string(),
                });
                break;
            }
            match defs.get(pid) {
                None => break, // ParentNotFound already reported
                Some(pd) => {
                    cur = pid;
                    parent = pd.def.parent.as_ref();
                }
            }
        }
        // A chain ending on `parent.is_none()` landed on the terminal
        // root `cur`; it should be the star. (Parentless non-star bodies
        // already reported as RootNotStar.)
        if b.def.parent.is_some() && parent.is_none() && cur != star_id {
            out.push(ValidationError::DisconnectedFromStar {
                body: b.def.id.as_str().to_string(),
            });
        }
    }
}

/// Field sanity on one body: finite where required, positive where
/// physics demands it.
fn check_body_fields(b: &crate::load::LoadedBody, out: &mut Vec<ValidationError>) {
    let body = b.def.id.as_str();
    check_positive(body, "radius_m", b.def.radius_m, out);
    check_positive(body, "mu_m3_per_s2", b.def.mu_m3_per_s2, out);
    check_finite(body, "tilt_rad", b.def.tilt_rad, out);
    match b.def.soi_m {
        Some(soi) => check_positive(body, "soi_m", soi, out),
        // `None` reads as infinite — the star's case (`BodyDef::soi_m`'s
        // doc). On a parented body an unbounded SOI swallows the whole
        // system: nothing inside can ever transition out, and the
        // child-side geometry rules in [`check_orbit_rules`] would have
        // no finite boundary to check against (silently skipped there
        // before this gate existed).
        None if b.def.parent.is_some() => out.push(ValidationError::SoiMissing {
            body: body.to_string(),
        }),
        None => {}
    }
    if let Some(rot) = b.def.rotation_period_s {
        check_positive(body, "rotation_period_s", rot, out);
    }
    if let Some(ins) = b.def.insolation_w_m2 {
        check_positive(body, "insolation_w_m2", ins, out);
    }
    if let Some(oc) = &b.def.ocean {
        check_positive(body, "ocean.datum_radius_m", oc.datum_radius_m, out);
    }
    check_file_refs(b, out);
}

/// Authored file references must be tame single-segment names — the
/// same gate [`crate::load`] runs before `join`ing them under the body
/// dir, repeated here so a hand-rolled `LoadedSystem` cannot smuggle a
/// `..`/absolute/separator-bearing ref to the renderer or the
/// concentration-map consumer.
fn check_file_refs(b: &crate::load::LoadedBody, out: &mut Vec<ValidationError>) {
    let body = b.def.id.as_str();
    let mut check = |field: &'static str, name: &str| {
        if !crate::load::authored_name_ok(name) {
            out.push(ValidationError::BadFileRef {
                body: body.to_string(),
                field,
            });
        }
    };
    if let Some(h) = &b.def.terrain.heightmap {
        check("terrain.heightmap", h);
    }
    if let Some(r) = &b.def.biome_map {
        check("biome_map.png", &r.png);
        check("biome_map.legend", &r.legend);
    }
    for (_, map) in &b.def.surface_resources {
        check("surface_resources.png", &map.png);
    }
}

/// Orbit-element sanity plus the §3.0 orbital rules: child apoapsis
/// inside parent SOI, `period_s` consistent with `2π√(a³/μ_parent)`
/// to [`PERIOD_REL_TOL`] relative, and the patched-conics geometry the
/// apoapsis rule alone does not cover — periapsis above the parent's
/// surface, the child's own SOI nested inside its own orbit, and
/// sibling SOIs that cannot overlap while their radial shells stay
/// disjoint.
fn check_orbit_rules(b: &crate::load::LoadedBody, defs: &DefMap, out: &mut Vec<ValidationError>) {
    let body = b.def.id.as_str();
    let Some(o) = &b.def.orbit else { return };
    check_positive(body, "orbit.semi_major_axis_m", o.semi_major_axis_m, out);
    check_finite(body, "orbit.inclination_rad", o.inclination_rad, out);
    check_finite(body, "orbit.ascending_node_rad", o.ascending_node_rad, out);
    check_finite(body, "orbit.arg_periapsis_rad", o.arg_periapsis_rad, out);
    check_finite(
        body,
        "orbit.mean_anomaly_epoch_rad",
        o.mean_anomaly_epoch_rad,
        out,
    );
    check_positive(body, "orbit.period_s", o.period_s, out);
    if !(0.0..1.0).contains(&o.eccentricity) {
        out.push(ValidationError::EccentricityOutOfRange {
            body: body.to_string(),
            e: o.eccentricity,
        });
    }

    // §3.0 rules need a resolvable, loaded parent.
    let Some(p) = b.def.parent.as_ref().and_then(|p| defs.get(p.as_str())) else {
        return;
    };
    let apoapsis = o.semi_major_axis_m * (1.0 + o.eccentricity);
    let periapsis = o.semi_major_axis_m * (1.0 - o.eccentricity);
    if let Some(soi) = p.def.soi_m
        && apoapsis >= soi
    {
        out.push(ValidationError::SoiTooSmall {
            parent: p.def.id.as_str().to_string(),
            child: body.to_string(),
            soi_m: soi,
            apoapsis_m: apoapsis,
        });
    }
    // A moon with a periapsis under the parent's surface is an authored
    // orbit through solid ground — `periapsis` is center-to-center, so
    // the clearance is both radii. (Non-finite `a`/`e` was already
    // reported above; NaN comparisons are false, so this never
    // double-reports.)
    let clearance = p.def.radius_m + b.def.radius_m;
    if periapsis <= clearance {
        out.push(ValidationError::PeriapsisInsideParent {
            parent: p.def.id.as_str().to_string(),
            child: body.to_string(),
            periapsis_m: periapsis,
            clearance_m: clearance,
        });
    }
    // The body's own SOI must nest strictly inside its own orbit:
    // `soi >= periapsis` means the SOI sphere reaches its parent at
    // closest approach — every vessel near the parent is then also
    // inside the child's SOI (and the §2.6.6 transition scan sees a
    // parent-inside-child contradiction).
    if let Some(soi) = b.def.soi_m
        && soi >= periapsis
    {
        out.push(ValidationError::SoiExceedsOrbit {
            body: body.to_string(),
            soi_m: soi,
            periapsis_m: periapsis,
        });
    }
    check_sibling_soi_overlap(b, defs, out);
    // T = 2π√(a³/μ_parent). libm::sqrt: f64::sqrt is banned by this
    // crate's clippy.toml (§8.2 determinism lints).
    let a = o.semi_major_axis_m;
    let computed = 2.0 * core::f64::consts::PI * libm::sqrt(a * a * a / p.def.mu_m3_per_s2);
    let rel = ((o.period_s - computed) / computed).abs();
    if rel > PERIOD_REL_TOL {
        out.push(ValidationError::PeriodInconsistent {
            body: body.to_string(),
            authored_s: o.period_s,
            computed_s: computed,
        });
    }
}

/// §3.0 sibling rule: two children of one parent whose SOI spheres can
/// overlap. The statically provable shape is *disjoint radial shells*:
/// while `[rp_x, apo_x]` and `[rp_y, apo_y]` do not overlap, the bodies'
/// center distance is bounded below by the shell gap regardless of
/// inclination, so `soi_x + soi_y > gap` means the spheres can meet at
/// closest approach (the §2.6.6 scan then sees a vessel inside two
/// sibling SOIs at once — an ambiguous transition). Overlapping shells
/// prove nothing statically — inclined orbits can share a radial range
/// without ever nearing — so those pairs are not flagged here.
///
/// Runs once per unordered pair: `b` reports only siblings whose id
/// sorts after its own. Siblings with `soi_m: None` (infinite) are
/// already flagged by the `SoiMissing` gate and skipped here.
fn check_sibling_soi_overlap(
    b: &crate::load::LoadedBody,
    defs: &DefMap,
    out: &mut Vec<ValidationError>,
) {
    let (Some(o), Some(soi)) = (&b.def.orbit, b.def.soi_m) else {
        return;
    };
    let rp = o.semi_major_axis_m * (1.0 - o.eccentricity);
    let apo = o.semi_major_axis_m * (1.0 + o.eccentricity);
    for s in defs.values() {
        if s.def.id == b.def.id || s.def.parent != b.def.parent {
            continue;
        }
        // Each unordered pair reports exactly once, under the sibling
        // whose id sorts first.
        if b.def.id.as_str() > s.def.id.as_str() {
            continue;
        }
        let (Some(so), Some(s_soi)) = (&s.def.orbit, s.def.soi_m) else {
            continue;
        };
        let s_rp = so.semi_major_axis_m * (1.0 - so.eccentricity);
        let s_apo = so.semi_major_axis_m * (1.0 + so.eccentricity);
        // The gap between the shells, positive only when they are
        // disjoint (the lower orbit's apoapsis below the higher's
        // periapsis); overlapping shells → not statically provable.
        let gap = (rp - s_apo).max(s_rp - apo);
        if gap > 0.0 && soi + s_soi > gap {
            out.push(ValidationError::SiblingSoiOverlap {
                parent: b
                    .def
                    .parent
                    .as_ref()
                    .map_or_else(String::new, |p| p.as_str().to_string()),
                a: b.def.id.as_str().to_string(),
                b: s.def.id.as_str().to_string(),
                gap_m: gap,
                soi_sum_m: soi + s_soi,
            });
        }
    }
}

/// §3.0 atmosphere model: positive model constants, γ > 1, the
/// `top_m > scale_height_m` rule, and a usable temperature table.
fn check_atmosphere(b: &crate::load::LoadedBody, out: &mut Vec<ValidationError>) {
    let Some(atm) = &b.def.atmosphere else { return };
    let body = b.def.id.as_str();
    check_positive(
        body,
        "atmosphere.sea_level_pressure_pa",
        atm.sea_level_pressure_pa,
        out,
    );
    check_positive(body, "atmosphere.scale_height_m", atm.scale_height_m, out);
    check_positive(body, "atmosphere.top_m", atm.top_m, out);
    check_positive(
        body,
        "atmosphere.molar_mass_kg_per_mol",
        atm.molar_mass_kg_per_mol,
        out,
    );
    if !(atm.heat_capacity_ratio.is_finite() && atm.heat_capacity_ratio > 1.0) {
        out.push(ValidationError::NonPositive {
            body: body.to_string(),
            field: "atmosphere.heat_capacity_ratio",
            value: atm.heat_capacity_ratio,
        });
    }
    if atm.top_m <= atm.scale_height_m {
        out.push(ValidationError::AtmosphereTopTooLow {
            body: body.to_string(),
            top_m: atm.top_m,
            scale_height_m: atm.scale_height_m,
        });
    }
    check_temperature_table(b, out);
}

fn check_temperature_table(b: &crate::load::LoadedBody, out: &mut Vec<ValidationError>) {
    let Some(atm) = &b.def.atmosphere else { return };
    let body = b.def.id.as_str();
    let mut fail = |reason: &'static str| {
        out.push(ValidationError::BadTemperatureTable {
            body: body.to_string(),
            reason,
        })
    };
    let knots = &atm.temperature_knots;
    if knots.len() < 2 {
        fail("needs at least 2 knots");
        return;
    }
    let mut prev = f64::NEG_INFINITY;
    for k in knots {
        if !k.altitude_m.is_finite() || !k.temperature_k.is_finite() {
            fail("non-finite knot");
            return;
        }
        if k.altitude_m <= prev {
            fail("altitudes not strictly increasing");
            return;
        }
        if k.altitude_m < 0.0 || k.altitude_m > atm.top_m {
            fail("knot outside [0, top_m]");
            return;
        }
        if k.temperature_k <= 0.0 {
            fail("non-positive temperature");
            return;
        }
        prev = k.altitude_m;
    }
}

fn check_biomes(
    b: &crate::load::LoadedBody,
    bm: &crate::load::LoadedBiomeMap,
    out: &mut Vec<ValidationError>,
) {
    let body = b.def.id.as_str();
    let mut indices = BTreeSet::new();
    let mut ids = BTreeSet::new();
    for e in &bm.legend.entries {
        check_id(e.id.as_str(), out);
        if !indices.insert(e.index) {
            out.push(ValidationError::BiomeLegendDuplicate {
                body: body.to_string(),
                detail: format!("index {}", e.index),
            });
        }
        if !ids.insert(e.id.as_str()) {
            out.push(ValidationError::BiomeLegendDuplicate {
                body: body.to_string(),
                detail: format!("id {:?}", e.id),
            });
        }
    }
    // Header pass for §3.0.1's dimension cap — `BiomeMapTooLarge`
    // carries the declared dims, which the decode's `&'static str`
    // cannot. [`decode_indexed_png`] enforces the same bound at header
    // parse regardless (pre-inflate), so an oversized map is rejected
    // either way and gets no pixel pass.
    if let Ok(info) = inspect_indexed_png(&bm.png)
        && (info.width > BIOME_MAP_MAX_W || info.height > BIOME_MAP_MAX_H)
    {
        out.push(ValidationError::BiomeMapTooLarge {
            body: body.to_string(),
            width: info.width,
            height: info.height,
        });
        return;
    }
    // Pixels, not just headers: a map that paints a palette index the
    // legend never names is an unnamed biome region, and a corrupt
    // IDAT is data rot — both surface only through the decode
    // ([`crate::biome::decode_indexed_png`] runs the same header rules
    // `inspect_indexed_png` did, plus inflate + palette bounds).
    match decode_indexed_png(&bm.png) {
        Err(reason) => out.push(ValidationError::BiomeMapPngInvalid {
            body: body.to_string(),
            reason,
        }),
        Ok(img) => {
            let info = img.info;
            for e in &bm.legend.entries {
                if usize::from(e.index) >= info.palette_len {
                    out.push(ValidationError::BiomeIndexExceedsPalette {
                        body: body.to_string(),
                        index: e.index,
                        palette_len: info.palette_len,
                    });
                }
            }
            // `indices` is exactly the legend's coverage set — every
            // pixel index must land in it. One error per uncovered
            // index, not per pixel.
            let mut reported = BTreeSet::new();
            for &i in &img.indices {
                if !indices.contains(&i) && reported.insert(i) {
                    out.push(ValidationError::BiomeIndexNotInLegend {
                        body: body.to_string(),
                        index: i,
                    });
                }
            }
        }
    }
}

/// §8.2's "ID stability"/loc-resolution check: every `LocKey` the system
/// references (`epoch_note`, display names, biome names) must resolve in
/// the locale file. Separate from [`validate_system`] because the loc
/// file lives outside the system directory.
pub fn validate_loc_keys(sys: &LoadedSystem, loc: &LocFile) -> Vec<ValidationError> {
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
    check("system.ron".to_string(), &sys.system.epoch_note);
    for b in &sys.bodies {
        check(format!("{}/body.ron", b.def.id), &b.def.display_name);
        if let Some(bm) = &b.biomes {
            for e in &bm.legend.entries {
                check(format!("{}/biomes.ron", b.def.id), &e.name);
            }
        }
    }
    out
}
