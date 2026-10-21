//! Golden scenario recipes (PLAN §8.3, §8.5).
//!
//! A recipe is a named deterministic driver: build the initial world, then
//! push commands through [`World::enqueue`]/[`World::step_frame`].
//!
//! **Recorder contract** — what makes the §7.6 command log self-contained
//! and reconstructible by `replay::replay`:
//!
//! - a frame's `step_frame` target equals its *drain tick* + 1 — the next
//!   advancing boundary, never a larger guess;
//! - every further advance flows through a logged `SimCommand::AdvanceTo`
//!   (§2.6.3), so the run's real targets are recoverable from the log
//!   itself;
//! - when `step_frame` returns [`AdvanceReport::partial`](csp_sim::AdvanceReport::partial)
//!   (§2.6.5 back-pressure), the same target is re-issued until reached —
//!   the budget bounds one call, never the run.
//!
//! A recipe that breaks the contract records fine but fails `record`'s
//! self-check: [`Replay::final_tick`](crate::replay::Replay::final_tick)
//! pins the run's end, so a stride the log cannot reproduce surfaces as a
//! `ClockMismatch`, not a silent divergence.

use csp_core::SimError;
use csp_sim::{SimCommand, SourceClass, World};

/// One recordable scenario: how to build the pristine world and how to
/// drive it. `initial` produces the world whose snapshot becomes the
/// artifact's initial snapshot; `drive` performs the run, and the resulting
/// [`World::command_log`] is the artifact body.
pub struct ScenarioRecipe {
    /// Scenario name — also its directory under `data/test-fixtures/` (§8.3).
    pub name: &'static str,
    /// The §2.7 save seed the run is recorded under. M0's `World` draws no
    /// seeded streams yet; the field rides the §7.6 artifact so replays
    /// carry it from day one.
    pub save_seed: u64,
    /// Builds the pristine pre-run world.
    pub initial: fn() -> World,
    /// Drives the run; propagates the first sim failure (§2.13).
    pub drive: fn(&mut World) -> Result<(), SimError>,
}

/// Every golden scenario, in registry order.
pub const SCENARIOS: &[ScenarioRecipe] = &[ScenarioRecipe {
    name: "trivial",
    save_seed: 0,
    initial: World::new,
    drive: drive_trivial,
}];

/// Look a recipe up by scenario name.
pub fn find(name: &str) -> Option<&'static ScenarioRecipe> {
    SCENARIOS.iter().find(|r| r.name == name)
}

/// The M0 trivial golden: a fresh world, ~1000 ticks, and `SetWarp`
/// commands from two source classes so the same-tick ordering rule
/// (§2.2.1) is exercised. The recorded log is:
/// - tick 0:   `Ui:0` `AdvanceTo(500)` — the first frame target;
/// - tick 500: `Ui:0` `SetWarp(4)`, `External:3` `SetWarp(16)` — one drain,
///   two sources;
/// - tick 501: `Ui:0` `AdvanceTo(1000)` — the run's end.
fn drive_trivial(w: &mut World) -> Result<(), SimError> {
    w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(500));
    w.step_frame(1)?; // drains at tick 0; the logged AdvanceTo drives to 500
    w.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(4));
    w.enqueue(SourceClass::External, 3, SimCommand::SetWarp(16));
    w.step_frame(501)?; // drains both at the tick-500 boundary, then one tick
    w.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(1000));
    w.step_frame(502)?; // drains at tick 501; AdvanceTo drives to 1000
    Ok(())
}
