//! The §3.5 `sites.ron` rules: schema band, unique IDs, body
//! membership, geocentric pose, launch-site cardinality,
//! `resource_yields` rows, the fixed-sun terminator margin, and the
//! site set's [`LocKey`] sweep.

use std::collections::{BTreeMap, BTreeSet};

use crate::load::LoadedSystem;

use crate::loc::LocFile;
use crate::resource::ResourceFile;
use crate::site::{SiteDef, SiteFile};
use crate::system::KeplerElements;

use super::ValidationError;
use super::common::check_id;
/// Minimum `|cos H · cos φ|` a site on a fixed-sun (zero-synodic)
/// body must keep from the permanent terminator — ~2.9° of real
/// angle at the equator. On such a body the Day/Night verdict is
/// decided ONCE by that product; a site within the floor lets a
/// floating-point residue flip its entire power budget (rask-mines
/// shipped at π/2 on a +6.1e-17 cosine — C09-F2). Non-synodic-zero
/// bodies cycle day/night anyway, so the rule only applies where the
/// verdict is permanent.
pub const FIXED_SUN_TERMINATOR_MIN_COS: f64 = 0.05;

/// The fixed-sun floor under `|ω − n_sun|` — mirrors `colonies::EPS`,
/// the `next_terminator_tick` "no crossings" bound (kept in step
/// manually: the sim depends on this crate, never the reverse).
const FIXED_SUN_SYNODIC_EPS: f64 = 1e-9;

/// §3.5's `sites.ron` checks: `schema_version` (the loader's
/// `current|previous` band), unique ids, every site on a declared
/// system body, a finite in-range geocentric pose, at most one launch
/// site per body AND at least one launch site in a non-empty file
/// (§3.5's "1.0 at the launch site" needs a definite site to mean it —
/// a file with sites but no pad validates green today while every
/// `ManufactureCraft` order is refused at runtime with a message that
/// never names the missing pad, C32-F5), and M8-B's `resource_yields`
/// rows — unique, known resources, finite fractions in `[0,1]`.
pub fn validate_sites(
    file: &SiteFile,
    sys: &LoadedSystem,
    res: &ResourceFile,
) -> Vec<ValidationError> {
    let mut out = Vec::new();
    if !crate::site::SITE_SCHEMA_SUPPORTED.contains(&file.schema_version) {
        out.push(ValidationError::SchemaVersion {
            file: "sites.ron".to_string(),
            found: file.schema_version,
            expected: crate::site::SITE_SCHEMA_VERSION,
        });
    }
    let bodies: BTreeSet<&str> = sys.bodies.iter().map(|b| b.def.id.as_str()).collect();
    let mut seen = BTreeSet::new();
    let mut launch_bodies = BTreeSet::new();
    for s in &file.sites {
        check_site(
            s,
            sys,
            &bodies,
            &mut seen,
            &mut launch_bodies,
            res,
            &mut out,
        );
    }
    // C32-F5's file-level floor: a NON-EMPTY site set must carry at
    // least one launch site — otherwise every `ManufactureCraft`
    // order is refused at runtime ("build orders consume a
    // launch-site inventory") and the pad-spawn path falls back to
    // lat/lon (0,0) alt 0, neither naming the missing pad. An
    // intentionally site-less system authors an empty file (the
    // loader's missing-file convention), never this state.
    if !file.sites.is_empty() && launch_bodies.is_empty() {
        out.push(ValidationError::InvalidSite {
            id: String::new(),
            reason: "no site carries launch: true — a non-empty sites.ron must \
                     author at least one launch site (a site-less system ships \
                     an empty sites list)"
                .to_string(),
        });
    }
    out
}

/// One `sites.ron` entry: id/version hygiene, the body membership and
/// geocentric-pose rules, the fixed-sun terminator guard, the
/// one-launch-site-per-body uniqueness accumulation, the
/// `resource_yields` rows, and the `starting_stock` rows.
#[allow(clippy::too_many_arguments)] // lint-allow (§8.6): the check list needs the shared tables it reads
fn check_site<'a>(
    s: &'a SiteDef,
    sys: &LoadedSystem,
    bodies: &BTreeSet<&str>,
    seen: &mut BTreeSet<&'a str>,
    launch_bodies: &mut BTreeSet<&'a str>,
    res: &ResourceFile,
    out: &mut Vec<ValidationError>,
) {
    let id = s.id.as_str();
    check_id(id, out);
    // The per-entry version is the legacy optional duplicate
    // (`SiteDef::schema_version`): a declared (nonzero) value gets the
    // same current-or-previous band as the file; `0` = "not declared",
    // the current authoring form.
    if s.schema_version != 0 && !crate::site::SITE_SCHEMA_SUPPORTED.contains(&s.schema_version) {
        out.push(ValidationError::SchemaVersion {
            file: format!("sites.ron entry {id:?}"),
            found: s.schema_version,
            expected: crate::site::SITE_SCHEMA_VERSION,
        });
    }
    if !seen.insert(id) {
        out.push(ValidationError::InvalidSite {
            id: id.to_string(),
            reason: "duplicate site id".to_string(),
        });
    }
    if !bodies.contains(s.body.as_str()) {
        out.push(ValidationError::InvalidSite {
            id: id.to_string(),
            reason: format!("body {:?} not in this system", s.body.as_str()),
        });
    }
    if !(s.latitude_rad.is_finite() && s.latitude_rad.abs() <= core::f64::consts::FRAC_PI_2) {
        out.push(ValidationError::InvalidSite {
            id: id.to_string(),
            reason: "latitude out of [-pi/2, pi/2] or non-finite".to_string(),
        });
    }
    if !s.longitude_rad.is_finite() {
        out.push(ValidationError::InvalidSite {
            id: id.to_string(),
            reason: "longitude non-finite".to_string(),
        });
    }
    if !(s.altitude_m.is_finite() && s.altitude_m >= 0.0) {
        out.push(ValidationError::InvalidSite {
            id: id.to_string(),
            reason: "altitude negative or non-finite".to_string(),
        });
    }
    // C09-F2/C34-F3's terminator-distance guard: on a zero-synodic
    // body the site's daylight is a PERMANENT verdict hung on
    // `cos H · cos φ` — a site authored near the line lets one
    // longitude ulp flip its whole power budget forever.
    // `None` covers an unresolved body (already flagged above) or
    // a non-fixed sun (the verdict cycles — grazing is legal).
    if let Some(margin) = fixed_sun_cos_margin(sys, s)
        && margin < FIXED_SUN_TERMINATOR_MIN_COS
    {
        out.push(ValidationError::InvalidSite {
            id: id.to_string(),
            reason: format!(
                "site sits {margin:.3e} of |cos H·cos φ| off a FIXED terminator \
                 (zero-synodic body {:?}) — the permanent Day/Night verdict must not \
                 hang on a rounding residue; keep ≥ {FIXED_SUN_TERMINATOR_MIN_COS}",
                s.body.as_str()
            ),
        });
    }
    if s.launch && !launch_bodies.insert(s.body.as_str()) {
        out.push(ValidationError::InvalidSite {
            id: id.to_string(),
            reason: format!(
                "second launch site on {:?} — one launch site per body",
                s.body.as_str()
            ),
        });
    }
    let mut yields = BTreeSet::new();
    for (r, y) in &s.resource_yields {
        if !yields.insert(r.as_str()) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!("duplicate yield resource {r:?}"),
            });
        }
        if !res.contains(r) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!("unknown yield resource {r:?}"),
            });
        }
        if !(y.is_finite() && (0.0..=1.0).contains(y)) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!("yield fraction for {r:?} outside [0,1] or non-finite"),
            });
        }
    }
    validate_site_stock(s, id, res, out);
}

/// W2.6: one site's `starting_stock` rows — the seeded stockpile
/// build orders and bench stands spend. Unique, known resources,
/// finite non-negative units (zero is pointless but legal — an
/// authored "this row exists" marker).
fn validate_site_stock(s: &SiteDef, id: &str, res: &ResourceFile, out: &mut Vec<ValidationError>) {
    let mut stock = BTreeSet::new();
    for (r, u) in &s.starting_stock {
        if !stock.insert(r.as_str()) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!("duplicate stock resource {r:?}"),
            });
        }
        if !res.contains(r) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!("unknown stock resource {r:?}"),
            });
        }
        if !(u.is_finite() && *u >= 0.0) {
            out.push(ValidationError::InvalidSite {
                id: id.to_string(),
                reason: format!("stock units for {r:?} negative or non-finite"),
            });
        }
    }
}

/// The site's `|cos H0 · cos φ|` terminator margin — `Some` only when
/// the body carries a FIXED sun (`|ω − n_sun|` under
/// [`FIXED_SUN_SYNODIC_EPS`], `csp-sim::colonies::next_terminator_tick`'s
/// no-crossings bound), where the product is a permanent Day/Night
/// verdict rather than a momentary phase. Replicates
/// `body_light`'s closed form — the sim depends on this crate, never
/// the reverse, so the predicate is re-derived rather than imported:
/// `n = 2π/T`, `L0 = M0 + ω_arg + Ω_node`, `H0 = λ − L0 − π`, and for
/// a tidally-locked body (`rotation_period_s` absent or ≤ 0 — spin IS
/// the orbital mean motion) the sun-direction sweep is the
/// star-orbiting ancestor's `(n, L0)`, walked up `parent` links while
/// parents still carry elements (a planet locked to the star itself
/// resolves to its own orbit — the genuinely fixed sun). A rotating
/// body keeps its own orbit's `n` — `body_light`'s non-locked path,
/// verbatim.
fn fixed_sun_cos_margin(sys: &LoadedSystem, s: &SiteDef) -> Option<f64> {
    const TWO_PI: f64 = 2.0 * core::f64::consts::PI;
    let bodies: BTreeMap<&str, &crate::system::BodyDef> = sys
        .bodies
        .iter()
        .map(|b| (b.def.id.as_str(), &b.def))
        .collect();
    let def = bodies.get(s.body.as_str())?;
    let motion = |d: &crate::system::BodyDef| -> (f64, f64) {
        d.orbit.as_ref().map_or((0.0, 0.0), |e: &KeplerElements| {
            let n = if e.period_s.is_finite() && e.period_s > 0.0 {
                TWO_PI / e.period_s
            } else {
                0.0
            };
            (
                n,
                e.mean_anomaly_epoch_rad + e.arg_periapsis_rad + e.ascending_node_rad,
            )
        })
    };
    let (n_body, _) = motion(def);
    let locked = !def.rotation_period_s.is_some_and(|p| p > 0.0);
    let omega = def
        .rotation_period_s
        .map_or(n_body, |p| if p > 0.0 { TWO_PI / p } else { n_body });
    let (n_sun, l0_sun) = if locked {
        // Walk the parent chain while parents still orbit — the walk
        // ends at the body that orbits the star directly (its parent
        // carries no elements). Bounded by the body count: parent
        // chains are `validate_system`-proven acyclic, and an
        // unvalidated cycle resolves to the last ancestor reached.
        let mut current = def;
        for _ in 0..bodies.len() {
            match current.parent.as_ref().and_then(|p| bodies.get(p.as_str())) {
                Some(parent) if parent.orbit.is_some() => current = parent,
                _ => break,
            }
        }
        motion(current)
    } else {
        let (_, l0) = motion(def);
        (n_body, l0)
    };
    if (omega - n_sun).abs() >= FIXED_SUN_SYNODIC_EPS {
        return None;
    }
    let h0 = s.longitude_rad - l0_sun - core::f64::consts::PI;
    Some((libm::cos(h0) * libm::cos(s.latitude_rad)).abs())
}

/// Loc resolution for the site set (the [`crate::validate::validate_loc_keys`]
/// pattern): every site's `display_name` must resolve in `en.ron`.
/// Separate from [`validate_sites`] because the loc file lives
/// outside the system directory.
pub fn validate_site_loc_keys(file: &SiteFile, loc: &LocFile) -> Vec<ValidationError> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for s in &file.sites {
        let key = &s.display_name;
        if seen.insert(key.as_str().to_string()) && !loc.contains(key) {
            out.push(ValidationError::UnresolvedLocKey {
                context: format!("site {}", s.id),
                key: key.as_str().to_string(),
            });
        }
    }
    out
}
