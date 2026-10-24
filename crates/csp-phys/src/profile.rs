//! The §3.4 `MissionProfile` — **stub in M3, real in M9b**.
//!
//! §3.4 defines the object the mission-confidence number is about:
//!
//! ```text
//! struct MissionProfile { burn_segments: Vec<(duration_s, throttle, Regime)> }
//! ```
//!
//! produced by the §2.11 design assistant, editable in the editor, and
//! consumed at M9b by the pure function `exp(−Λ_expected(MissionProfile))`
//! — expected hazard integrated over the profile's burn segments
//! against each part's resolved stress rules.
//!
//! What M3 needs from it is narrower: the VAB readout compares the
//! craft's computed stage Δv against a *target* budget and the launch
//! TWR against a floor — "meets Terrin LEO budget Y/N". So the stub
//! carries the real `burn_segments` field verbatim (the shape M9b
//! reads) plus the editor-facing requirement numbers; the
//! [`MissionProfile::evaluate`] check is a plain comparison, not the
//! hazard model — that is deliberately deferred.
//!
//! Serialized per the session-domain rule: a profile may ride craft
//! files later, so serde derives are on now, but it is *not* part of
//! the `CSPCRAFT` schema at v1 (session.rs's codec is unchanged).

use serde::{Deserialize, Serialize};

use crate::readout::CraftAnalysis;

/// §3.4's burn-segment regime tag — the minimal honest taxonomy for M3:
/// which stress environment a segment runs in. The stress rules it will
/// feed at M9b key on atmosphere presence (`Overtemperature`,
/// `ExcessG`), vacuum operation, and surface contact; three regimes
/// cover that split without inventing M9b's full set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Regime {
    /// Vacuum operation — orbit insertion, transfer, landing burn on
    /// an airless body.
    Vacuum,
    /// In-atmosphere flight — ascent/descent legs where aero and
    /// thermal stress apply.
    Atmosphere,
    /// Landed or surface-latched — dormant/low-stress except contact.
    Surface,
}

/// One §3.4 burn segment: `throttle` is the commanded fraction and
/// `duration_s` how long it holds. At M9b the pair drives `Λ_expected`
/// per regime; at M3 it is carried data plus editor display.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BurnSegment {
    /// Burn duration (s).
    pub duration_s: f64,
    /// Commanded throttle fraction ∈ [0, 1].
    pub throttle: f64,
    /// The segment's regime.
    pub regime: Regime,
}

/// The mission profile the VAB checks the working craft against.
/// Defaults are [`MissionProfile::terrin_ascent`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MissionProfile {
    /// Display name for the profile preset.
    pub name: String,
    /// Total Δv the mission must fit (m/s) — compared against
    /// [`CraftAnalysis::total_delta_v_m_per_s`]. For an ascent preset
    /// this includes the loss allowance the data set was tuned around
    /// (Terrin LEO is ~2 287 m/s circular; ascent craft budget ~3.1–3.4
    /// km/s once gravity and drag are paid — csp-data's parts test).
    pub target_delta_v_m_per_s: f64,
    /// Minimum first-stage TWR the launchpad can accept — the KSP
    /// ">1 or it doesn't leave the pad" rule measured against
    /// `reference_g_m_s2`.
    pub min_launch_twr: f64,
    /// Surface gravity the TWR comparison is against (m/s²). Defaults
    /// to `csp_data::G0` — Terrin's authored surface gravity, so the
    /// default profile's TWR check equals the readout's `twr_surface`.
    pub reference_g_m_s2: f64,
    /// §3.4's `burn_segments` verbatim — the field M9b's confidence
    /// number integrates over. Carried now so the editor object is
    /// already the plan's shape; not consumed by M3's `evaluate`.
    pub burn_segments: Vec<BurnSegment>,
}

/// The readout-vs-profile comparison the VAB panel shows — a stub
/// verdict, not the §3.4 confidence number.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MissionCheck {
    /// `total Δv − target Δv` (m/s) — positive is margin.
    pub delta_v_margin_m_per_s: f64,
    /// `margin ≥ 0`.
    pub meets_delta_v: bool,
    /// First-fired stage's TWR vs `reference_g_m_s2` — `None` when the
    /// craft fires no engines at all.
    pub launch_twr: Option<f64>,
    /// `launch_twr ≥ min_launch_twr` (vacuously false with no engines).
    pub meets_launch_twr: bool,
    /// Both checks pass.
    pub meets_profile: bool,
}

impl MissionProfile {
    /// The default preset: a Terrin ascent budget. 3 400 m/s is the
    /// top of the starter stack's Δv band (csp-data's parts test puts
    /// the single-stage number at ~3 096 m/s — "reaches LEO once losses
    /// are paid"), so this profile demands a comfortably-staged craft.
    /// `min_launch_twr` 1.2 is the usual genre floor for ascent.
    pub fn terrin_ascent() -> Self {
        Self {
            name: "Terrin ascent (LEO)".to_string(),
            target_delta_v_m_per_s: 3_400.0,
            min_launch_twr: 1.2,
            reference_g_m_s2: csp_data::G0,
            burn_segments: vec![
                BurnSegment {
                    duration_s: 120.0,
                    throttle: 1.0,
                    regime: Regime::Atmosphere,
                },
                BurnSegment {
                    duration_s: 180.0,
                    throttle: 1.0,
                    regime: Regime::Vacuum,
                },
            ],
        }
    }

    /// Compare `analysis` against the profile — the M3 stub verdict.
    /// The first-fired engine stage supplies `launch_twr` (the
    /// launchpad number); its `twr_surface` was computed at `G0`, so
    /// for a non-Terrin `reference_g_m_s2` the thrust/mass ratio is
    /// rescaled here rather than reusing the displayed figure
    /// (`twr = F/(m·g)` is linear in `1/g`).
    pub fn evaluate(&self, analysis: &CraftAnalysis) -> MissionCheck {
        let margin = analysis.total_delta_v_m_per_s - self.target_delta_v_m_per_s;
        let launch_twr = analysis
            .stages
            .iter()
            .find(|s| !s.engine_ids.is_empty())
            .map(|s| s.twr_surface * csp_data::G0 / self.reference_g_m_s2.max(1e-9));
        let meets_dv = margin >= 0.0;
        let meets_twr = launch_twr.is_some_and(|t| t >= self.min_launch_twr);
        MissionCheck {
            delta_v_margin_m_per_s: margin,
            meets_delta_v: meets_dv,
            launch_twr,
            meets_launch_twr: meets_twr,
            meets_profile: meets_dv && meets_twr,
        }
    }
}

impl Default for MissionProfile {
    fn default() -> Self {
        Self::terrin_ascent()
    }
}

// ---------------------------------------------------------------------------
// M9b: the real confidence number — `exp(−Λ_expected(profile))` (§3.4).
// ---------------------------------------------------------------------------

/// §3.4's expected hazard over a [`MissionProfile`], evaluated per
/// part spec — the pure function the confidence number is `exp` of.
///
/// The evaluation is a *conservative approximation* (documented —
/// the live model's exact regime inputs do not exist at planning
/// time): per burn segment, each part's `λ_base · qual_factor(family)`
/// picks up every authored stress rule whose *planning* metric the
/// segment trips:
///
/// - `OverThrottle` — `seg.throttle > threshold`.
/// - `BurnBeyondRated` — the cumulative burn duration through the
///   segment exceeds `engine.rated_burn_s + threshold` (engine specs
///   only; evaluated against the running total, not per segment).
/// - `Overtemperature` — regime `Atmosphere` (ascent/descent heating
///   is the authored stress case).
/// - `ExcessG` — regime `Atmosphere` (the high-g legs are the
///   atmospheric ones).
/// - `UllageNegativeIgnition` — regime `Vacuum` on a
///   `ullage_required` engine (vacuum ignition is assumed
///   free-fall).
/// - `ExcessRestarts` — segment index `> threshold` (each segment
///   past the threshold counts as a restart).
///
/// Infant mortality is not folded in — a planned mission reads the
/// mature hazard (the fresh-part bump is a property of a *specific*
/// unit's usage, not of the profile). Dormant hazard between
/// segments is likewise outside the profile's scope — the profile
/// covers its burn segments only (the §3.4 shape is burn-segmented;
/// coast time is the design assistant's to add if it ever authors
/// dormant legs).
///
/// `quals` supplies the live per-family qualification records —
/// absent families resolve at the unqualified end of the curve,
/// matching the resolver's `FamilyQualification::FRESH` convention.
///
/// Returns the Λ sum only; use [`expected_hazard_report`] for the
/// skipped-spec count (specs that fail [`ReliabilitySpec::is_valid`]
/// or carry a non-finite qualification record are excluded, never
/// silently — the report says how many).
pub fn expected_hazard(
    specs: &[crate::resolve::ReliabilitySpec],
    quals: &crate::resolve::FamilyQualifications,
    profile: &MissionProfile,
) -> f64 {
    expected_hazard_report(specs, quals, profile).lambda
}

/// The [`expected_hazard`] result plus accounting: which inputs the
/// evaluation *skipped* rather than silently dropped (R14-F1).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HazardReport {
    /// `Λ_expected` — the integrated hazard over the profile's burn
    /// segments for the valid specs.
    pub lambda: f64,
    /// Specs excluded from `lambda`: `is_valid` failures and specs
    /// whose live qualification record reads non-finite or negative
    /// units. A display should surface this — an invalid spec used to
    /// vanish into the total with no trace.
    pub skipped_specs: usize,
}

/// [`expected_hazard`] with the skipped-spec accounting — same
/// evaluation, plus a count of the inputs it refused.
pub fn expected_hazard_report(
    specs: &[crate::resolve::ReliabilitySpec],
    quals: &crate::resolve::FamilyQualifications,
    profile: &MissionProfile,
) -> HazardReport {
    let mut lambda_total = 0.0;
    let mut skipped_specs = 0usize;
    for spec in specs {
        if !spec.is_valid() {
            skipped_specs += 1;
            continue;
        }
        // The live-qualification read must be finite and non-negative:
        // a corrupt record (NaN/±∞ units) would otherwise evaluate at
        // the curve's *last* (best) knot — every `units ≤ knot`
        // comparison fails on NaN — optimistically under-counting the
        // part's hazard. Skip-and-count, same as an invalid spec.
        let units = quals.get(&spec.family).map_or(0.0, |q| q.units());
        if !units.is_finite() || units < 0.0 {
            skipped_specs += 1;
            continue;
        }
        let qual = crate::resolve::qual_factor(&spec.qualification_curve, units);
        // `is_valid` does not inspect the carried curve's knots; a
        // malformed one can still yield a non-finite factor — gate the
        // read the same way the resolver's qual_factor output gate does.
        if !qual.is_finite() {
            skipped_specs += 1;
            continue;
        }
        let base = spec.base_hazard_per_s() * qual;
        let mut burn_s = 0.0;
        for (i, seg) in profile.burn_segments.iter().enumerate() {
            if !seg.duration_s.is_finite() || seg.duration_s < 0.0 {
                continue;
            }
            let mut stress = 1.0;
            for rule in &spec.regime_stress {
                let active = match rule.kind {
                    csp_data::StressKind::OverThrottle => seg.throttle > rule.threshold,
                    csp_data::StressKind::BurnBeyondRated => {
                        spec.engine.is_some()
                            && burn_s + seg.duration_s
                                > rule.threshold + spec.engine.map_or(0.0, |e| e.rated_burn_s)
                    }
                    csp_data::StressKind::Overtemperature => seg.regime == Regime::Atmosphere,
                    csp_data::StressKind::ExcessG => seg.regime == Regime::Atmosphere,
                    csp_data::StressKind::UllageNegativeIgnition => {
                        seg.regime == Regime::Vacuum
                            && spec.engine.is_some_and(|e| e.ullage_required)
                    }
                    csp_data::StressKind::ExcessRestarts => i as f64 > rule.threshold,
                };
                if active {
                    stress *= rule.multiplier;
                }
            }
            lambda_total += base * stress * seg.duration_s;
            if spec.engine.is_some() {
                burn_s += seg.duration_s;
            }
        }
    }
    HazardReport {
        lambda: lambda_total,
        skipped_specs,
    }
}

/// §3.4's mission confidence — `exp(−Λ_expected(MissionProfile))`,
/// the pure function (the M3 `evaluate` stub stays for the Δv/TWR
/// verdicts; this is the reliability number the profile object was
/// defined for).
pub fn confidence(
    specs: &[crate::resolve::ReliabilitySpec],
    quals: &crate::resolve::FamilyQualifications,
    profile: &MissionProfile,
) -> f64 {
    libm::exp(-expected_hazard(specs, quals, profile))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::readout::StageStats;

    fn analysis(dv: f64, twr: f64) -> CraftAnalysis {
        CraftAnalysis {
            effective: Vec::new(),
            stages: vec![StageStats {
                stage: 0,
                engine_ids: vec![crate::graph::PartNodeId(0)],
                delta_v_m_per_s: dv,
                twr_surface: twr,
                ..StageStats::default()
            }],
            total_delta_v_m_per_s: dv,
            ..CraftAnalysis::default()
        }
    }

    #[test]
    fn profile_check_reports_margin_and_twr() {
        let p = MissionProfile::terrin_ascent();
        let good = p.evaluate(&analysis(3_600.0, 1.5));
        assert!(good.meets_profile);
        assert!((good.delta_v_margin_m_per_s - 200.0).abs() < 1e-9);
        let short = p.evaluate(&analysis(3_000.0, 1.5));
        assert!(!short.meets_delta_v && !short.meets_profile);
        let weak = p.evaluate(&analysis(3_600.0, 1.0));
        assert!(!weak.meets_launch_twr && !weak.meets_profile);
        // No engines → no launch TWR → fails the TWR leg.
        let mut no_eng = analysis(9_999.0, 0.0);
        no_eng.stages[0].engine_ids.clear();
        let c = p.evaluate(&no_eng);
        assert!(c.launch_twr.is_none() && !c.meets_profile);
    }

    /// R14-F1: invalid specs and corrupt qualification records are
    /// *counted*, not silently dropped — and a NaN/±∞ units read can
    /// no longer evaluate at the curve's best knot.
    #[test]
    fn hazard_report_counts_skipped_specs() {
        use crate::resolve::{FamilyQualifications, ReliabilitySpec};
        // An invalid spec (zero MTBF) is skipped and reported.
        let mut bad = ReliabilitySpec {
            family: csp_data::FamilyId::from("f"),
            base_mtbf_s: 0.0,
            infant_mortality: 0.0,
            dormant_stress: 0.0,
            regime_stress: Vec::new(),
            failure_modes: vec![(csp_data::FailureMode::SensorFault, 1.0)],
            qualification_curve: csp_data::QualCurve {
                knots: vec![csp_data::QualKnot {
                    qualification_units: 0.0,
                    hazard_multiplier: 1.0,
                }],
            },
            engine: None,
        };
        let quals = FamilyQualifications::new();
        let p = MissionProfile::terrin_ascent();
        let r = expected_hazard_report(std::slice::from_ref(&bad), &quals, &p);
        assert_eq!(r.skipped_specs, 1);
        assert_eq!(r.lambda, 0.0);
        // A valid spec whose family record carries NaN units skips
        // too — it must not read the curve's best knot.
        bad.base_mtbf_s = 100.0;
        let mut quals = FamilyQualifications::new();
        quals.insert(
            csp_data::FamilyId::from("f"),
            crate::resolve::FamilyQualification {
                flight_units: f64::NAN,
                ..crate::resolve::FamilyQualification::FRESH
            },
        );
        let r = expected_hazard_report(&[bad], &quals, &p);
        assert_eq!(r.skipped_specs, 1);
        assert_eq!(r.lambda, 0.0);
    }

    /// A non-Terrin reference g rescales the launchpad TWR — the
    /// readout keeps its G0-based number, the profile check divides by
    /// the profile's own g.
    #[test]
    fn reference_g_rescales_twr() {
        let mut p = MissionProfile::terrin_ascent();
        p.reference_g_m_s2 = csp_data::G0 * 2.0; // twice Terrin
        p.min_launch_twr = 0.8;
        let c = p.evaluate(&analysis(4_000.0, 1.5));
        assert!((c.launch_twr.unwrap() - 0.75).abs() < 1e-9);
        assert!(!c.meets_launch_twr);
    }
}
