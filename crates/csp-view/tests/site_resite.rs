//! Surface-site re-siting probes (M5 §5.2-R2) — the authoring tool
//! that ships with the terrain. Whenever a body's `terrain` block
//! changes (amplitude/relief/seed), the baked field under every
//! authored site moves; these probes measure the NEW field so
//! `sites.ron` can be re-authored against it:
//!
//! * `pad_resite_scan` — flatness scan of a lon/lat grid around a
//!   site's CURRENT coordinates: per candidate it prints lon, lat,
//!   the datum-relative field height, and the max−min flatness of the
//!   rendered surface over a 30 m footprint ring, then the flattest
//!   candidates above the dryness floor. Authoring rule: within the
//!   scan window flatness beats proximity — pick the flattest
//!   candidate (target ≤ ±0.5 m over the ring) and author
//!   `altitude_m` = the field height there. Sites validate
//!   `altitude_m ≥ 0`, so a candidate needs a non-negative field
//!   height to be authorable at all.
//! * `site_field_audit` — every authored site against its body's new
//!   field: field height, the ring-max ground anchor
//!   (`ground_radius`'s ±tap ring at the same 30 m footprint the
//!   structures anchor samples), the authored `altitude_m`, the
//!   buried/floating delta, and the ring flatness — the report that
//!   decides which non-pad sites need re-siting.
//!
//! Both bake the body's REAL field — `HeightField::bake_profiled` at
//! the authored `ReliefProfile`'s dims, the same bake `TerrainCache`
//! ships to the GPU — and sample `sample_dir` on body-fixed lon/lat
//! directions (the texel-exact CPU mirror of the vertex shader's
//! `height_at`; the `pad_spawn::launch_site_sits_on_rendered_ground`
//! convention). `#[ignore]`d like the render dumps — run explicitly:
//!
//! ```text
//! cargo test -p csp-view --test site_resite -- --ignored --nocapture
//! ```
//!
//! Env knobs (`pad_resite_scan`): `RESITE_BODY` (default `terrin`),
//! `RESITE_LON`/`RESITE_LAT` (scan center; default the body's launch
//! site), `RESITE_SPAN_DEG` (half-window, default 5), `RESITE_STEP_DEG`
//! (default 0.25), `RESITE_MIN_DRY_M` (default 10).
#![cfg(not(target_arch = "wasm32"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csp_core::DVec3;
use csp_view::View;

/// The body's real baked field plus its scale and datum — everything a
/// site measurement needs. The `(seed, profile)` bake is the one
/// `TerrainCache::field` runs per frame; `amplitude_m` is the authored
/// `radius · amplitude_frac` every consumer (contact surface, camera
/// floor, structures, shader) shares.
struct BakedBody {
    radius_m: f64,
    amplitude_m: f64,
    field: csp_render::terrain::HeightField,
}

/// Bake `body`'s authored field: terrain params from the view's
/// `TerrainConfig` (the `config_from_bodies` mapping the renderer
/// uses), radius from the authored `body.ron`.
fn baked(view: &View, radius_m: f64, body: &str) -> BakedBody {
    let cfg = view.world.resource::<csp_view::terrain::TerrainConfig>();
    let t = cfg
        .bodies
        .get(body)
        .unwrap_or_else(|| panic!("no terrain config for {body}"));
    BakedBody {
        radius_m,
        amplitude_m: t.amplitude_m,
        field: csp_render::terrain::HeightField::bake_profiled(t.seed, t.profile),
    }
}

/// The authored system's body radii — `data/systems/home`'s `body.ron`
/// set through the same loader the game boots with.
fn home_radii() -> std::collections::BTreeMap<String, f64> {
    let sys = csp_data::load_system(&csp_view::data_root().join("systems/home")).unwrap();
    sys.bodies
        .iter()
        .map(|b| (b.def.id.as_str().to_owned(), b.def.radius_m))
        .collect()
}

/// Body-fixed unit direction at `(lat, lon)` — the same spherical
/// form `bubble::surface_pose_restore` rebuilds poses with.
fn dir_at(lat: f64, lon: f64) -> DVec3 {
    let (sl, cl) = lat.sin_cos();
    let (so, co) = lon.sin_cos();
    DVec3::new(cl * co, cl * so, sl)
}

/// Datum-relative surface heights (m) at `dir` and eight taps on the
/// `half_span_m`-radius ring around it — the same ±tap ring
/// `ground_radius` maxes over; kept per-sample here so the probe reads
/// flatness (max − min), not just the anchor bound. A 15 m half-span
/// is the 30 m pad footprint `emit_launch_complex` anchors on.
fn ring_heights(b: &BakedBody, dir: DVec3, half_span_m: f64) -> Vec<f64> {
    let tap = half_span_m / b.radius_m;
    let east = {
        let e = DVec3::Z.cross(dir);
        if e.norm_squared() > 1.0e-12 {
            e / e.norm()
        } else {
            DVec3::X
        }
    };
    let north = dir.cross(east);
    let mut hs = vec![b.field.sample_dir(dir) * b.amplitude_m];
    for k in 0..8 {
        let a = f64::from(k) * core::f64::consts::FRAC_PI_4;
        let d = dir + east * (a.cos() * tap) + north * (a.sin() * tap);
        hs.push(b.field.sample_dir(d / d.norm()) * b.amplitude_m);
    }
    hs
}

/// `(center height, ring-max anchor, ring flatness)` in metres.
fn probe(b: &BakedBody, lat: f64, lon: f64) -> (f64, f64, f64) {
    let hs = ring_heights(b, dir_at(lat, lon), 15.0);
    let anchor = hs.iter().copied().fold(f64::MIN, f64::max);
    let flat = anchor - hs.iter().copied().fold(f64::MAX, f64::min);
    (hs[0], anchor, flat)
}

fn env_f(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// The pad re-siting scan. Run with `--ignored --nocapture` (see the
/// module doc); env knobs override the center/window/step so the same
/// probe re-sites any site on any body after a terrain change.
#[test]
#[ignore]
fn pad_resite_scan() {
    let view = View::new().expect("data/ load");
    let body = std::env::var("RESITE_BODY").unwrap_or_else(|_| "terrin".into());
    let radius_m = *home_radii()
        .get(&body)
        .unwrap_or_else(|| panic!("no body.ron radius for {body}"));
    let field = baked(&view, radius_m, &body);
    // Default center: the body's launch site (the pad this scan exists
    // to re-site); RESITE_LON/RESITE_LAT center anywhere else (e.g. a
    // colony site on a body with no launch site — then (0, 0) is the
    // fallback unless overridden).
    let site = view
        .sim
        .config()
        .sites
        .into_values()
        .find(|s| s.launch && s.body.as_str() == body);
    let (lat0, lon0) = (
        env_f("RESITE_LAT", site.as_ref().map_or(0.0, |s| s.latitude_rad)),
        env_f("RESITE_LON", site.as_ref().map_or(0.0, |s| s.longitude_rad)),
    );
    let span = env_f("RESITE_SPAN_DEG", 5.0).to_radians();
    let step = env_f("RESITE_STEP_DEG", 0.25).to_radians();
    let min_dry = env_f("RESITE_MIN_DRY_M", 10.0);
    let (h0, anchor0, flat0) = probe(&field, lat0, lon0);
    println!(
        "scan {body}: center lon={lon0:+.5} lat={lat0:+.5} field_h={h0:+.2} m \
         anchor={anchor0:+.2} m flat30={flat0:.3} m; span {:.1}° step {:.2}° min_dry {min_dry} m",
        span.to_degrees(),
        step.to_degrees(),
    );
    let n = (span / step) as i64;
    let mut dry = Vec::new();
    let mut total = 0usize;
    for i in -n..=n {
        for j in -n..=n {
            let lat = lat0 + i as f64 * step;
            if lat.abs() > core::f64::consts::FRAC_PI_2 {
                continue;
            }
            let lon = lon0 + j as f64 * step;
            let (h, anchor, flat) = probe(&field, lat, lon);
            let (lon_d, lat_d) = (lon.to_degrees(), lat.to_degrees());
            total += 1;
            println!(
                "cand lon={lon:+.5} ({lon_d:+7.2}°) lat={lat:+.5} ({lat_d:+7.2}°) h={h:+9.2} anchor={anchor:+9.2} flat30={flat:8.3}"
            );
            if h >= min_dry {
                dry.push((
                    flat,
                    (lat - lat0).abs() + (lon - lon0).abs(),
                    lat,
                    lon,
                    h,
                    anchor,
                ));
            }
        }
    }
    dry.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.total_cmp(&b.1)));
    println!(
        "{total} candidates, {} at h ≥ {min_dry} m — flattest (then nearest):",
        dry.len()
    );
    for (flat, _, lat, lon, h, anchor) in dry.iter().take(30) {
        println!(
            "best lon={lon:+.5} lat={lat:+.5} h={h:+9.2} anchor={anchor:+9.2} flat30={flat:8.3}"
        );
    }
}

/// The every-site audit — the buried/floating report that decides
/// which non-pad sites (recovery points, colony hosts) need
/// re-siting, and the non-negativity check the site validator's
/// `altitude_m ≥ 0` rule and colony founding impose on any re-site.
#[test]
#[ignore]
fn site_field_audit() {
    let view = View::new().expect("data/ load");
    let radii = home_radii();
    let mut cache: std::collections::BTreeMap<String, BakedBody> = Default::default();
    for site in view.sim.config().sites.values() {
        let body = site.body.as_str().to_owned();
        let baked_body = match cache.get(&body) {
            Some(b) => b,
            None => {
                let b = baked(
                    &view,
                    *radii
                        .get(&body)
                        .unwrap_or_else(|| panic!("no radius for {body}")),
                    &body,
                );
                cache.insert(body.clone(), b);
                cache.get(&body).unwrap()
            }
        };
        let (h, anchor, flat) = probe(baked_body, site.latitude_rad, site.longitude_rad);
        let (id, lon, lat) = (site.id.as_str(), site.longitude_rad, site.latitude_rad);
        let alt = site.altitude_m;
        let delta = h - alt;
        let note = if h < 0.0 {
            "  <-- NEGATIVE: altitude_m >= 0 unsatisfiable here"
        } else {
            ""
        };
        println!(
            "{id:<18} body={body:<7} lon={lon:+.5} lat={lat:+.5} field_h={h:+9.2} \
             anchor30={anchor:+9.2} authored_alt={alt:+9.2} delta={delta:+9.2} \
             flat30={flat:8.3}{note}",
        );
    }
}
