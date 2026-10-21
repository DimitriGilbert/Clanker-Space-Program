//! bevy_ecs presentation world and the six named schedules (PLAN §2.5).
//!
//! Authoritative sim state never lives in this `World` — it mirrors `csp-sim`
//! snapshots for presentation. `bevy_ecs` is confined to this crate
//! (`default-features = false`), so replacing it later is cheap.

use bevy_ecs::prelude::*;
use csp_core::SimTime;

/// Integer sim clock mirrored for presentation.
#[derive(Resource, Default)]
pub struct Clock(pub SimTime);

/// Proof-of-life stats shown in the debug panel.
#[derive(Resource, Default)]
pub struct FrameStats {
    pub frames: u64,
    pub ticks_last_frame: u64,
}

/// One fixed sim tick. Runs inside `SimStep`, 0..=N times per frame.
/// Single-threaded executor, always (PLAN §2.5).
fn advance_clock(mut clock: ResMut<Clock>) {
    clock.0.advance(1);
}

/// The six schedules from PLAN §2.5, run explicitly by `csp-app`.
pub struct Schedules {
    /// winit events -> SimCommands; camera input; UI event intake.
    pub input: Schedule,
    /// The sim's own step function. Never parallel.
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

/// The presentation world + its schedules.
pub struct View {
    pub world: World,
    pub schedules: Schedules,
}

impl View {
    pub fn new() -> Self {
        let mut world = World::new();
        world.insert_resource(Clock::default());
        world.insert_resource(FrameStats::default());

        let mut sim_step = Schedule::default();
        sim_step.add_systems(advance_clock);

        Self {
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

    /// Advance the sim by `n` fixed ticks (physics-warp path, PLAN §2.6).
    pub fn sim_ticks(&mut self, n: u64) {
        for _ in 0..n {
            self.schedules.sim_step.run(&mut self.world);
        }
        self.world.resource_mut::<FrameStats>().ticks_last_frame = n;
    }

    /// Run the once-per-frame schedules in order.
    pub fn frame(&mut self) {
        for s in [
            &mut self.schedules.input,
            &mut self.schedules.sync,
            &mut self.schedules.presentation,
            &mut self.schedules.ui_build,
            &mut self.schedules.render_extract,
        ] {
            s.run(&mut self.world);
        }
        self.world.resource_mut::<FrameStats>().frames += 1;
    }

    pub fn clock(&self) -> SimTime {
        self.world.resource::<Clock>().0
    }
}

impl Default for View {
    fn default() -> Self {
        Self::new()
    }
}
