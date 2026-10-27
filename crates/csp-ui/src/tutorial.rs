//! M10 — the first-flight tutorial (PLAN M10 exit row:
//! "tutorial/first-flight").
//!
//! A guided, dismissible checklist that walks a new player through the
//! vertical slice — open the VAB, assemble a flyable rocket, launch,
//! throttle up, climb, reach space, close the orbit. It is **not** a
//! scripted slideshow: every step's completion is a predicate over
//! [`TutorialInput`], a plain-data snapshot `csp-view`'s
//! `tutorial_drive` system fills from the *real* staged mirrors each
//! frame (VAB session analysis, the flight HUD mirrors, the osculating
//! apsides computed off the staged vessel state). The tutorial
//! advances only when the player actually did the thing.
//!
//! Session state (§2.2 category C): `csp-view` owns the
//! [`TutorialState`] resource; this module owns the model — the step
//! table, the progress logic, and the overlay panel — as pure data +
//! pure egui, the same split as [`crate::vab`]'s session model.

/// One tutorial step's presentation text. Progress is positional —
/// the step's *predicate* lives in [`step_complete`], not here, so the
/// table stays a pure list.
#[derive(Clone, Copy, Debug)]
pub struct TutorialStep {
    /// One-line checklist title ("Launch from the pad").
    pub title: &'static str,
    /// The current-step coaching line shown under the checklist. A
    /// `{key}` placeholder is filled from the live keymap at draw
    /// time (see [`Self::hint_for`]) so a rebind never leaves the
    /// coaching line naming a key that does nothing.
    pub hint: &'static str,
    /// The action a `{key}` placeholder in [`Self::hint`] resolves
    /// against — `None` for a hint with no key reference.
    pub key_action: Option<crate::settings::KeyAction>,
}

impl TutorialStep {
    /// The coaching line with `{key}` replaced by the action's live
    /// binding display — `"[m]"`, `"[unbound]"` when the action has
    /// no keys.
    fn hint_for(
        &self,
        keys: &crate::settings::Keymap,
        labels: &std::collections::BTreeMap<String, String>,
    ) -> String {
        match self.key_action {
            Some(action) => self
                .hint
                .replace("{key}", &format!("[{}]", keys.hint_labeled(action, labels))),
            None => self.hint.to_string(),
        }
    }
}

/// Index of the LAUNCH step — the rewind anchor: when a vessel leaves
/// the bubble mid-flight-steps (crash, impact removal) the tutorial
/// steps back to it rather than stalling on telemetry that can never
/// arrive.
pub const STEP_LAUNCH: usize = 2;

/// Index of the first science-tier step — the boundary the
/// vessel-loss rewind keys on. The flight tier above it measures
/// live telemetry only a surviving vessel produces (throttle,
/// altitude, closed apsides); this tier's predicates read the
/// program ledger — pooled, banked, spent — which outlives the ship
/// that earned it (C23-F3). Must index [`STEPS`]' "Run a science
/// experiment" entry.
pub const SCIENCE_TIER_START: usize = 7;

/// The climb milestone altitude (m).
pub const CLIMB_ALT_M: f64 = 10_000.0;

/// "Space" altitude on an airless body (m) — `atmosphere_top_m` is the
/// real boundary when the home world carries an atmosphere; an
/// airless one keeps the same coaching line honest.
pub const SPACE_FALLBACK_ALT_M: f64 = 70_000.0;

/// The first-flight checklist, in order. `STEP_LAUNCH` must index
/// [`STEPS`]' launch entry.
pub const STEPS: &[TutorialStep] = &[
    TutorialStep {
        title: "Open the Vehicle Assembly Building",
        hint: "Open the VAB — {key} or the VAB button in the top bar.",
        key_action: Some(crate::settings::KeyAction::ToggleVab),
    },
    TutorialStep {
        title: "Assemble a flyable rocket",
        hint: "Stack a command pod, a fuel tank, and an engine — the \
               readout must show thrust.",
        key_action: None,
    },
    TutorialStep {
        title: "Launch from the pad",
        hint: "Press LAUNCH in the top bar — your craft spawns on the pad.",
        key_action: None,
    },
    TutorialStep {
        title: "Open the throttle",
        hint: "Spool the engine — {key} for full throttle.",
        key_action: Some(crate::settings::KeyAction::ThrottleFull),
    },
    TutorialStep {
        title: "Climb to 10 km",
        hint: "Hold the burn and climb — 10 km on the altimeter.",
        key_action: None,
    },
    TutorialStep {
        title: "Reach space",
        hint: "Keep burning until you clear the atmosphere (70 km on Terrin).",
        key_action: None,
    },
    TutorialStep {
        title: "Close the orbit",
        hint: "Pitch over to prograde near apoapsis and raise periapsis \
               above the atmosphere.",
        key_action: None,
    },
    // Second tier — the science loop (W1.5-c): the starter craft ships
    // a canister + battery, so this tier is flyable out of the box.
    TutorialStep {
        title: "Run a science experiment",
        hint: "Open the Program window — {key} — and press 'run' on the \
               canister's experiment under SCIENCE.",
        key_action: Some(crate::settings::KeyAction::OpenProgram),
    },
    TutorialStep {
        title: "Bank the data",
        hint: "Transmit the pooled record (drains ElectricCharge), or \
               land and 'recover' it for the full yield.",
        key_action: None,
    },
    TutorialStep {
        title: "Research a tech node",
        hint: "Spend banked science on a RESEARCH node in the Program \
               window — {key}.",
        key_action: Some(crate::settings::KeyAction::OpenProgram),
    },
];

/// The progress snapshot `csp-view` stages each frame — plain data,
/// derived from real sim mirrors, never sim types (§2.1).
#[derive(Clone, Copy, Debug, Default)]
pub struct TutorialInput {
    /// The VAB editor is open.
    pub in_vab: bool,
    /// Working-craft part count (the VAB session's node count).
    pub craft_parts: usize,
    /// The resolved analysis shows engine thrust — a *flyable* craft,
    /// not just a part.
    pub craft_has_thrust: bool,
    /// A focused vessel exists in the bubble.
    pub has_vessel: bool,
    /// Focused vessel's throttle `[0,1]`.
    pub throttle: f64,
    /// Focused vessel's altitude above the surface (m).
    pub alt_m: f64,
    /// Osculating apoapsis altitude (m) — `None` while open/unknown.
    pub apoapsis_m: Option<f64>,
    /// Osculating periapsis altitude (m) of a *closed* trajectory —
    /// `None` while open/unknown.
    pub periapsis_m: Option<f64>,
    /// The home atmosphere's top (m); `0` on airless bodies.
    pub atmosphere_top_m: f64,
    /// Pooled science-data units aboard the focused vessel — proof an
    /// experiment ran (staged from the Program mirror).
    pub science_pooled: f64,
    /// The program's banked science total — proof a transmit/recovery
    /// landed.
    pub science_banked: f64,
    /// A nonzero-cost tech node is researched — proof the player spent
    /// science (free roots are excluded so the baseline doesn't count).
    pub research_spent: bool,
}

/// One step's completion predicate — the tutorial's honesty rule made
/// code: a step completes only on real state, never on a button press.
/// Steps a player can legitimately bypass (a craft that arrived
/// another way — the stock fallback, a loaded file) complete on the
/// *outcome* too: a flying vessel is an assembled rocket.
fn step_complete(step: usize, i: &TutorialInput) -> bool {
    match step {
        0 => i.in_vab || i.has_vessel,
        1 => i.has_vessel || (i.craft_parts > 0 && i.craft_has_thrust),
        2 => i.has_vessel,
        3 => i.has_vessel && i.throttle > 0.0,
        4 => i.has_vessel && i.alt_m > CLIMB_ALT_M,
        5 => i.has_vessel && i.alt_m > space_alt(i),
        6 => i.periapsis_m.is_some_and(|p| p > space_alt(i)),
        // Science tier — banked science also satisfies "ran an
        // experiment" for the skip-ahead player who already cashed it.
        7 => i.science_pooled > 0.0 || i.science_banked > 0.0,
        8 => i.science_banked > 0.0,
        9 => i.research_spent,
        _ => false,
    }
}

/// The altitude the "reach space"/"orbit" steps measure against —
/// the atmosphere's top when the body has one, the fallback boundary
/// otherwise.
fn space_alt(i: &TutorialInput) -> f64 {
    if i.atmosphere_top_m > 0.0 {
        i.atmosphere_top_m
    } else {
        SPACE_FALLBACK_ALT_M
    }
}

/// The tutorial session model (§2.2-C): which step the checklist sits
/// on and the flags the overlay draws. Progress is *derived* —
/// [`Self::observe`] folds each frame's [`TutorialInput`] into it; the
/// overlay's buttons mutate only `active`/`step` (dismiss, restart),
/// never the completion record.
#[derive(Clone, Debug)]
pub struct TutorialState {
    /// The overlay is on screen. `false` after Dismiss (or before the
    /// top bar re-opens it) — progress still derives while hidden so a
    /// re-opened tutorial shows the truth.
    pub active: bool,
    /// Index into [`STEPS`] — `STEPS.len()` means every step completed.
    pub step: usize,
    /// Every step completed — the overlay switches to the
    /// congratulation card.
    pub finished: bool,
    /// A vessel the tutorial was tracking left the bubble while the
    /// pending step still needed one — the overlay's "vessel lost"
    /// banner; the step rewound to the tier's resumable checkpoint
    /// ([`STEP_LAUNCH`] on the flight tier, [`SCIENCE_TIER_START`]
    /// on the science tier).
    pub vessel_lost: bool,
    /// Peak altitude observed (m) — the overlay's progress readout.
    pub best_alt_m: f64,
    /// Best closed-orbit periapsis observed (m) — `f64::MIN` until a
    /// closed trajectory is seen.
    pub best_periapsis_m: f64,
}

impl Default for TutorialState {
    /// A new session starts with the tutorial on screen — the M10
    /// "first flight" greeting.
    fn default() -> Self {
        Self {
            active: true,
            step: 0,
            finished: false,
            vessel_lost: false,
            best_alt_m: 0.0,
            best_periapsis_m: f64::MIN,
        }
    }
}

impl TutorialState {
    /// Fold one frame's staged progress snapshot into the checklist.
    ///
    /// - *advance first*: every step whose predicate the snapshot
    ///   already satisfies completes before a vessel loss is judged —
    ///   a player who skipped ahead sees the checklist catch up, and
    ///   the coached "land and 'recover'" banks pooled science while
    ///   removing the ship in the same sim apply, so that frame
    ///   (`has_vessel: false`, banked science on the ledger) must
    ///   complete the pending step, not rewind it (C23-F3);
    /// - *rewind*: a lost vessel steps back only to the checkpoint
    ///   the coached path can still resume from — [`STEP_LAUNCH`]
    ///   while a telemetry step is pending, [`SCIENCE_TIER_START`]
    ///   when the ship went down with an unbanked record — and sets
    ///   `vessel_lost`. Banked science carries the science tier
    ///   vessel-free, so a deliberate recovery is progress, not a
    ///   loss;
    /// - *latch*: `finished` is sticky — a completed tutorial is not
    ///   un-completed by a later crash.
    pub fn observe(&mut self, input: &TutorialInput) {
        if input.has_vessel {
            self.vessel_lost = false;
        }
        self.best_alt_m = self.best_alt_m.max(input.alt_m);
        if let Some(p) = input.periapsis_m {
            self.best_periapsis_m = self.best_periapsis_m.max(p);
        }
        if self.finished {
            return;
        }
        self.advance(input);
        if self.finished {
            return;
        }
        // The rewind fires only when progress needs a replacement
        // vessel: a pending telemetry step always does — its
        // predicate reads live flight state — while a pending
        // science-tier step does only with an empty ledger (banked
        // science satisfies the early tier and funds the research
        // step from the Program window, no ship required). The
        // checkpoint is the tier's own resumable step, never a full
        // reset: relaunch for the flight tier; the tier's first step
        // for the science tier, where any replacement vessel — even
        // a pad hopper that never re-flies the ascent — re-runs the
        // experiment.
        if !input.has_vessel
            && self.step > STEP_LAUNCH
            && (self.step < SCIENCE_TIER_START || input.science_banked <= 0.0)
        {
            self.step = if self.step < SCIENCE_TIER_START {
                STEP_LAUNCH
            } else {
                SCIENCE_TIER_START
            };
            self.vessel_lost = true;
            // The rewind may have landed on a step the ledger still
            // satisfies — re-advance so the checklist never displays
            // a step the snapshot already passed.
            self.advance(input);
        }
    }

    /// Run the positional advance loop and latch `finished` —
    /// factored out because [`Self::observe`] applies it before AND
    /// after the loss rewind: the coached recovery's snapshot must
    /// complete its pending step before the loss is judged.
    fn advance(&mut self, input: &TutorialInput) {
        while self.step < STEPS.len() && step_complete(self.step, input) {
            self.step += 1;
        }
        if self.step >= STEPS.len() {
            self.finished = true;
            self.vessel_lost = false;
        }
    }

    /// The step the checklist is coaching, `None` once finished.
    pub fn current(&self) -> Option<&'static TutorialStep> {
        STEPS.get(self.step)
    }

    /// Re-open the checklist (the top bar's Tutorial button) —
    /// progress is kept, so a dismissed tutorial resumes where it was.
    pub fn start(&mut self) {
        self.active = true;
    }

    /// Restart the checklist from step zero — the overlay's restart
    /// button; `finished`/`vessel_lost` clear, the bests stay (they are
    /// history, not progress).
    pub fn restart(&mut self) {
        self.step = 0;
        self.finished = false;
        self.vessel_lost = false;
        self.active = true;
    }

    /// Hide the checklist for the rest of the session.
    pub fn dismiss(&mut self) {
        self.active = false;
    }
}

/// The tutorial overlay — a checklist card drawn in every mode (it
/// coaches the VAB steps from inside the editor too). Pure egui over
/// the session model: `state` mutates only through the Dismiss/Restart
/// buttons; progress arrives via [`TutorialState::observe`], never
/// through this function. `keys` is the live keymap the coaching
/// lines' `{key}` placeholders resolve against.
///
/// The parking spot is mode-aware: the VAB's Craft inspector owns
/// the right-top corner, so `in_vab` drops the card to center-top
/// where it overlaps nothing; flight/map park left-top — the
/// right-top slot collides with the map's orbit readout chip
/// (C23-F6). Once shown it is a normal window — the player drags it
/// by the title bar wherever it bothers least.
/// Returns the drawn window rect — the host masks the VAB's pointer
/// input against it (the default-on card sits over the build area).
pub fn tutorial_overlay(
    ctx: &egui::Context,
    state: &mut TutorialState,
    keys: &crate::settings::Keymap,
    key_labels: &std::collections::BTreeMap<String, String>,
    in_vab: bool,
) -> egui::Rect {
    let screen = ctx.content_rect();
    let pos = if in_vab {
        egui::pos2(screen.center().x - 145.0, screen.top() + 30.0)
    } else {
        // Left-top, under the top bar — the right-top corner belongs
        // to the map overlay's orbit readout (C23-F6). Same row the
        // Planning window uses; both are draggable once parked.
        egui::pos2(screen.left() + 12.0, screen.top() + 60.0)
    };
    egui::Window::new("First flight")
        .default_pos(pos)
        .collapsible(true)
        .resizable(false)
        .default_width(290.0)
        .show(ctx, |ui| {
            if state.finished {
                ui.label(
                    egui::RichText::new(
                        "Program airborne — orbit closed, science banked, research started.",
                    )
                    .strong()
                    .color(egui::Color32::from_rgb(120, 220, 120)),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "The Program window ({}) is home now: contracts, research, vessel science.",
                        keys.hint_labeled(crate::settings::KeyAction::OpenProgram, key_labels)
                    ))
                    .color(egui::Color32::from_gray(170)),
                );
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("Dismiss").clicked() {
                        state.dismiss();
                    }
                    if ui.button("Restart").clicked() {
                        state.restart();
                    }
                });
                return;
            }
            let done = egui::Color32::from_rgb(120, 220, 120);
            let cur = egui::Color32::from_rgb(255, 220, 90);
            let todo = egui::Color32::from_gray(130);
            for (i, s) in STEPS.iter().enumerate() {
                let (mark, color) = if i < state.step {
                    ("✔", done)
                } else if i == state.step {
                    ("→", cur)
                } else {
                    ("·", todo)
                };
                ui.label(egui::RichText::new(format!("{mark}  {}", s.title)).color(color));
            }
            if let Some(step) = state.current() {
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("Now: {}", step.hint_for(keys, key_labels)))
                        .color(egui::Color32::from_gray(200)),
                );
            }
            if state.vessel_lost {
                ui.label(
                    egui::RichText::new("vessel lost — build or load a craft and LAUNCH again")
                        .color(egui::Color32::from_rgb(255, 130, 90)),
                );
            }
            if state.best_alt_m > 1.0 {
                ui.label(
                    egui::RichText::new(format!("best altitude: {:.1} km", state.best_alt_m / 1e3))
                        .color(egui::Color32::from_gray(150)),
                );
            }
            if state.best_periapsis_m > f64::MIN {
                ui.label(
                    egui::RichText::new(format!(
                        "best periapsis: {:.1} km",
                        state.best_periapsis_m / 1e3
                    ))
                    .color(egui::Color32::from_gray(150)),
                );
            }
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("Dismiss").clicked() {
                    state.dismiss();
                }
                if ui.button("Restart").clicked() {
                    state.restart();
                }
            });
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING)
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers the
// shipping fallible paths.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// A scripted player: each element is one frame's snapshot. The
    /// tutorial must advance exactly when the scripted action's *real*
    /// outcome appears — the whole point of the progress-detection
    /// design.
    fn run_script(frames: &[TutorialInput]) -> TutorialState {
        let mut t = TutorialState::default();
        for f in frames {
            t.observe(f);
        }
        t
    }

    #[test]
    fn full_progression_completes_in_order() {
        let idle = TutorialInput::default();
        let mut t = TutorialState::default();
        // Nothing done — nothing completes.
        t.observe(&idle);
        assert_eq!(t.step, 0);
        // Open the VAB.
        t.observe(&TutorialInput {
            in_vab: true,
            ..idle
        });
        assert_eq!(t.step, 1);
        // A craft with thrust resolves — Assemble completes; the
        // launch hasn't happened yet, so it stops there.
        t.observe(&TutorialInput {
            in_vab: true,
            craft_parts: 3,
            craft_has_thrust: true,
            ..idle
        });
        assert_eq!(t.step, 2);
        // On the pad, throttle still zero.
        t.observe(&TutorialInput {
            has_vessel: true,
            atmosphere_top_m: 70_000.0,
            ..idle
        });
        assert_eq!(t.step, 3);
        // Throttle up — but the checklist does not credit climb yet.
        t.observe(&TutorialInput {
            has_vessel: true,
            throttle: 1.0,
            alt_m: 200.0,
            atmosphere_top_m: 70_000.0,
            ..idle
        });
        assert_eq!(t.step, 4);
        // Mid-climb past 10 km.
        t.observe(&TutorialInput {
            has_vessel: true,
            throttle: 1.0,
            alt_m: 15_000.0,
            atmosphere_top_m: 70_000.0,
            ..idle
        });
        assert_eq!(t.step, 5);
        // Space.
        t.observe(&TutorialInput {
            has_vessel: true,
            alt_m: 80_000.0,
            apoapsis_m: Some(120_000.0),
            periapsis_m: Some(-5_000.0),
            atmosphere_top_m: 70_000.0,
            ..idle
        });
        assert_eq!(t.step, 6);
        // Orbit closed.
        t.observe(&TutorialInput {
            has_vessel: true,
            alt_m: 90_000.0,
            apoapsis_m: Some(120_000.0),
            periapsis_m: Some(80_000.0),
            atmosphere_top_m: 70_000.0,
            ..idle
        });
        assert_eq!(t.step, 7);
        // The experiment pooled data aboard.
        t.observe(&TutorialInput {
            has_vessel: true,
            science_pooled: 10.0,
            atmosphere_top_m: 70_000.0,
            ..idle
        });
        assert_eq!(t.step, 8);
        // Transmitted/recovered into the ledger.
        t.observe(&TutorialInput {
            has_vessel: true,
            science_banked: 4.0,
            atmosphere_top_m: 70_000.0,
            ..idle
        });
        assert_eq!(t.step, 9);
        // Spent on a paid tech node.
        t.observe(&TutorialInput {
            has_vessel: true,
            science_banked: 4.0,
            research_spent: true,
            atmosphere_top_m: 70_000.0,
            ..idle
        });
        assert!(t.finished);
        assert!(t.current().is_none());
    }

    /// Skipping ahead is honest: a player who already launched (stock
    /// fallback, loaded file) sees the early steps complete at once —
    /// the checklist measures outcomes, not button order.
    #[test]
    fn already_flying_skips_early_steps() {
        let t = run_script(&[TutorialInput {
            has_vessel: true,
            throttle: 1.0,
            alt_m: 25_000.0,
            atmosphere_top_m: 70_000.0,
            ..TutorialInput::default()
        }]);
        assert_eq!(t.step, 5); // 0–4 all satisfied by the snapshot
    }

    /// The rewind rule: losing the vessel mid-flight steps back to the
    /// launch step with the lost banner, and the checklist resumes
    /// when a new vessel flies.
    #[test]
    fn vessel_loss_rewinds_to_launch() {
        let mut t = run_script(&[TutorialInput {
            has_vessel: true,
            throttle: 1.0,
            alt_m: 15_000.0,
            atmosphere_top_m: 70_000.0,
            ..TutorialInput::default()
        }]);
        assert_eq!(t.step, 5);
        // The vessel is gone (breakup, impact removal).
        t.observe(&TutorialInput::default());
        assert_eq!(t.step, STEP_LAUNCH);
        assert!(t.vessel_lost);
        // A fresh launch clears the banner and re-advances.
        t.observe(&TutorialInput {
            has_vessel: true,
            atmosphere_top_m: 70_000.0,
            ..TutorialInput::default()
        });
        assert!(!t.vessel_lost);
        assert_eq!(t.step, 3);
    }

    /// C23-F3 — the coached "land and 'recover'" path banks the
    /// pooled record and removes the vessel in the same sim apply,
    /// so the post-recovery frame reads `has_vessel: false` with
    /// banked science on the ledger. That frame must ADVANCE the
    /// checklist — the ledger satisfies the pending step vessel-free
    /// — not rewind it to launch under a spurious "vessel lost"
    /// banner.
    #[test]
    fn coached_recovery_advances_instead_of_rewinding() {
        let idle = TutorialInput::default();
        // Fly the flight tier and pool data aboard — the checklist
        // parks on "Bank the data".
        let mut t = run_script(&[TutorialInput {
            has_vessel: true,
            throttle: 1.0,
            alt_m: 90_000.0,
            periapsis_m: Some(80_000.0),
            atmosphere_top_m: 70_000.0,
            science_pooled: 10.0,
            ..idle
        }]);
        assert_eq!(t.step, 8);
        // Landed and recovered: the bank landed, the vessel is gone.
        t.observe(&TutorialInput {
            science_banked: 4.0,
            atmosphere_top_m: 70_000.0,
            ..idle
        });
        assert_eq!(t.step, 9, "banked science must carry the tier");
        assert!(!t.vessel_lost, "recovery is the coached path, not a loss");
        // The research node needs no ship — buying it finishes.
        t.observe(&TutorialInput {
            science_banked: 4.0,
            research_spent: true,
            ..idle
        });
        assert!(t.finished);
        assert!(!t.vessel_lost);
    }

    /// C23-F3 — the verifier's second shape: the checklist sat on the
    /// research step when the spent flag arrived on a vessel-free
    /// frame (the ship was recovered first, then the node bought —
    /// draining the ledger to zero). The advance must complete the
    /// tier before the empty-ledger rewind is judged; judging the
    /// loss first would reopen a finished checklist.
    #[test]
    fn research_spent_on_vessel_free_frame_finishes() {
        let idle = TutorialInput::default();
        let mut t = run_script(&[TutorialInput {
            has_vessel: true,
            throttle: 1.0,
            alt_m: 90_000.0,
            periapsis_m: Some(80_000.0),
            atmosphere_top_m: 70_000.0,
            science_pooled: 10.0,
            science_banked: 4.0,
            ..idle
        }]);
        assert_eq!(t.step, 9);
        t.observe(&TutorialInput {
            research_spent: true,
            ..idle
        });
        assert!(t.finished);
        assert!(!t.vessel_lost);
    }

    /// C23-F3 — the empty-ledger complement: a vessel lost mid
    /// science-tier with nothing banked can't progress on the ledger,
    /// so the checklist steps back — but only to the tier's first
    /// step, where ANY replacement vessel resumes it. Rewinding to
    /// LAUNCH would re-demand the whole ascent and dead-end a pad
    /// science run behind the telemetry steps.
    #[test]
    fn unbanked_loss_rewinds_to_science_tier_start() {
        let idle = TutorialInput::default();
        let mut t = run_script(&[TutorialInput {
            has_vessel: true,
            throttle: 1.0,
            alt_m: 90_000.0,
            periapsis_m: Some(80_000.0),
            atmosphere_top_m: 70_000.0,
            science_pooled: 10.0,
            ..idle
        }]);
        assert_eq!(t.step, 8);
        // The ship went down with the record aboard — ledger empty.
        t.observe(&idle);
        assert_eq!(t.step, SCIENCE_TIER_START);
        assert!(t.vessel_lost);
        // A pad hopper re-runs the experiment: the checkpoint is
        // resumable without re-flying the ascent.
        t.observe(&TutorialInput {
            has_vessel: true,
            science_pooled: 6.0,
            atmosphere_top_m: 70_000.0,
            ..idle
        });
        assert!(!t.vessel_lost);
        assert_eq!(t.step, 8);
        // Transmit banks it; the ship then burns up on the way home —
        // banked science still carries the tier, no rewind.
        t.observe(&TutorialInput {
            science_banked: 4.0,
            ..idle
        });
        assert_eq!(t.step, 9);
        assert!(!t.vessel_lost);
    }

    /// The tier constants must index the table entries they name —
    /// `step_complete` and the loss rewind key on both.
    #[test]
    fn tier_constants_index_their_steps() {
        assert_eq!(STEPS[STEP_LAUNCH].title, "Launch from the pad");
        assert_eq!(STEPS[SCIENCE_TIER_START].title, "Run a science experiment");
    }

    /// `finished` is a latch — a post-orbit crash does not reopen the
    /// checklist.
    #[test]
    fn finished_is_sticky() {
        let mut t = run_script(&[TutorialInput {
            has_vessel: true,
            throttle: 1.0,
            alt_m: 90_000.0,
            periapsis_m: Some(80_000.0),
            atmosphere_top_m: 70_000.0,
            science_banked: 4.0,
            research_spent: true,
            ..TutorialInput::default()
        }]);
        assert!(t.finished);
        t.observe(&TutorialInput::default());
        assert!(t.finished);
        assert!(!t.vessel_lost);
    }

    /// Dismiss hides; the top-bar reopen resumes where the checklist
    /// was; restart clears progress but keeps the recorded bests.
    #[test]
    fn dismiss_reopen_restart() {
        let mut t = run_script(&[TutorialInput {
            in_vab: true,
            craft_parts: 3,
            craft_has_thrust: true,
            alt_m: 30_000.0,
            periapsis_m: Some(10_000.0),
            ..TutorialInput::default()
        }]);
        assert_eq!(t.step, 2);
        t.dismiss();
        assert!(!t.active);
        t.start();
        assert!(t.active);
        assert_eq!(t.step, 2); // progress kept
        t.restart();
        assert!(t.active);
        assert_eq!(t.step, 0);
        assert!(!t.finished);
        assert_eq!(t.best_alt_m, 30_000.0); // history survives restart
    }
}
