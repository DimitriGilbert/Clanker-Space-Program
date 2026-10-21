//! bevy_ecs presentation world and the six named schedules (PLAN §2.5).
//!
//! Authoritative sim state never lives in this `World` — it mirrors `csp-sim`
//! for presentation. `bevy_ecs` is confined to this crate
//! (`default-features = false`), so replacing it later is cheap.

use bevy_ecs::prelude::*;
use csp_sim::World as SimWorld;

/// Integer sim clock mirrored for presentation — a COPY, written each frame by
/// the Sync bridge. Never authoritative (PLAN §2.2).
#[derive(Resource, Default)]
pub struct Clock(pub csp_core::SimTime);

/// Proof-of-life stats shown in the debug panel.
#[derive(Resource, Default)]
pub struct FrameStats {
    pub frames: u64,
    pub ticks_last_frame: u64,
}

/// The six schedules from PLAN §2.5, run explicitly by `csp-app`.
pub struct Schedules {
    /// winit events -> SimCommands; camera input; UI event intake.
    pub input: Schedule,
    /// The sim's own step — single-threaded always (PLAN §2.5).
    pub sim_step: Schedule,
    /// Sim snapshot -> presentation components.
    pub sync: Schedule,
    /// Interpolation, camera solve, LOD, VFX, audio cues.
    pub presentation: Schedule,
    /// egui pass; emits SimCommands consumed next frame.
    pub ui_build: Schedule,
    /// Build the immutable `csp_render::Frame`.
    pub render_extract: Schedule,
}

/// The presentation world + its schedules + the authoritative sim.
pub struct View {
    /// Authoritative sim — lives OUTSIDE the ECS (PLAN §2.2).
    pub sim: SimWorld,
    pub world: World,
    pub schedules: Schedules,
}

impl View {
    pub fn new() -> Self {
        let mut world = World::new();
        world.insert_resource(Clock::default());
        world.insert_resource(FrameStats::default());

        let mut sim_step = Schedule::default();
        sim_step.set_executor(bevy_ecs::schedule::SingleThreadedExecutor::new());

        Self {
            sim: SimWorld::new(),
            world,
            schedules: Schedules {
                input: Schedule::default(),
                sim_step,
                sync: Schedule::default(),
                presentation: Schedule::default(),
                ui_build: Schedule::default(),
                render_extract: Schedule::default(),
            },
        }
    }

    /// Once per frame: Input.
    pub fn begin_frame(&mut self) {
        self.schedules.input.run(&mut self.world);
    }

    /// Advance the sim by `n` fixed ticks (physics-warp path, PLAN §2.6).
    /// Per tick: the authoritative `sim.step()` THEN the `SimStep` schedule —
    /// in that order, and never the other way around.
    pub fn sim_ticks(&mut self, n: u64) {
        for _ in 0..n {
            self.sim.step();
            self.schedules.sim_step.run(&mut self.world);
        }
        self.world.resource_mut::<FrameStats>().ticks_last_frame = n;
    }

    /// Once per frame, after all sim ticks: the Sync bridge writes the
    /// presentation mirror, then Presentation, UiBuild, RenderExtract.
    /// (PLAN §2.5 ordering: Input → SimStep → Sync → Presentation →
    /// UiBuild → RenderExtract.)
    pub fn end_frame(&mut self) {
        // Sync: authoritative -> presentation mirror.
        self.world.resource_mut::<Clock>().0 = self.sim.clock();
        self.schedules.sync.run(&mut self.world);
        self.schedules.presentation.run(&mut self.world);
        // NOTE: the egui pass itself is the UiBuild phase — csp-app runs it
        // between these two schedule calls so UI sees post-step state.
        self.schedules.ui_build.run(&mut self.world);
        self.schedules.render_extract.run(&mut self.world);
        self.world.resource_mut::<FrameStats>().frames += 1;
    }
}

impl Default for View {
    fn default() -> Self {
        Self::new()
    }
}
