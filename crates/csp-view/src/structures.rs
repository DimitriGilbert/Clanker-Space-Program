//! Static launch-site structures — the space-center layer.
//!
//! The authored `sites.ron` launch site is where the world stops being
//! bare terrain: this module emits the pad (a raised launch mount
//! straddling a flame trench), the service gantry, the VAB, the
//! crawlerway, a propellant farm, and support buildings into the
//! flight view's lit vertex stream, anchored to the *rendered* terrain
//! surface (the same baked heightfield the sim contacts — see
//! `crate::terrain`), so the craft reads as parked at a real space
//! center rather than floating over empty ground.
//!
//! # Frames
//!
//! Everything is authored in the site's local ENU frame (fixed frame):
//! +x east, +y north, +z up, the origin at the site's ground point.
//! [`site_frame`] bakes the §2.6.4 spin convention
//! (`inertial = R(axis, ω·t)·fixed`) plus `csp_sim::bubble`'s
//! `surface_upright` ENU basis into one fixed→inertial rotation, and
//! each structure's own ground anchor re-samples the heightfield under
//! its footprint (max over a ring, skirted) so buildings sit on the
//! terrain instead of floating or sinking. All f64 world math; the
//! only f32 is the clip-space cast at emission (§2.3).
//!
//! # Data flow (§2.5)
//!
//! [`SiteTable`] is a data-derived constant built once at
//! `View::with_sim` — the `map::BodyTable` precedent (sites never
//! mutate; `config()`'s full-table clone is a startup cost, never a
//! per-frame one). `extract_flight` reads it and emits into the same
//! lit `FlightVertex` stream the craft uses, so the structures take
//! the frame's staged sun/atmosphere lighting for free. The map view
//! draws the launch site as a ring marker + label when its camera is
//! focused on the site body.

use bevy_ecs::prelude::*;
use csp_core::{DQuat, DVec3};

use crate::flight::FlightEnv;
use crate::map::project_into;
use crate::part_mesh::SmoothTri;
use crate::terrain::{TerrainCache, TerrainConfig};

/// One authored surface site — the `sites.ron` slice the structure
/// layer and the map marker need.
#[derive(Clone, Debug)]
pub struct SiteEntry {
    /// The site's stable id (`"terrin-launch"`).
    pub id: csp_data::SiteId,
    /// Display-name loc key — the map label resolves it through
    /// `crate::LocStrings`.
    pub display_name: csp_data::LocKey,
    /// The body the site sits on.
    pub body: csp_data::BodyId,
    /// Geocentric latitude (rad).
    pub latitude_rad: f64,
    /// Longitude, eastward from the fixed frame's +x (rad).
    pub longitude_rad: f64,
    /// Altitude above the datum radius (m).
    pub altitude_m: f64,
    /// `true` for the body's launch site — the space-center complex.
    /// Other sites are recovery points; they get a map marker only.
    pub launch: bool,
}

/// The authored site table — a data-derived constant (§7.3) built at
/// `View::with_sim`, the `map::BodyTable`/`OrbitModelRes` precedent.
/// `with_sim` worlds with no sites installed simply carry an empty
/// table and render no structures.
#[derive(Resource, Default)]
pub struct SiteTable {
    /// Every authored site, `BTreeMap` (deterministic) order.
    pub sites: Vec<SiteEntry>,
}

impl SiteTable {
    /// Snapshot `sim`'s site table at construction — one `config()`
    /// read, at startup where its whole-config clone cost belongs.
    pub(crate) fn from_sim(sim: &csp_sim::World) -> Self {
        Self {
            sites: sim
                .config()
                .sites
                .into_values()
                .map(|s| SiteEntry {
                    id: s.id,
                    display_name: s.display_name,
                    body: s.body,
                    latitude_rad: s.latitude_rad,
                    longitude_rad: s.longitude_rad,
                    altitude_m: s.altitude_m,
                    launch: s.launch,
                })
                .collect(),
        }
    }

    /// The launch site on `body`, if one is authored (the validator
    /// enforces at most one per body).
    pub fn launch_site(&self, body: &csp_data::BodyId) -> Option<&SiteEntry> {
        self.sites.iter().find(|s| s.launch && s.body == *body)
    }
}

/// The site's placement in the inertial bubble frame: the ground
/// anchor (the terrain surface point under the site) plus the ENU
/// basis every structure shares, so the whole complex stays coherent.
pub(crate) struct SiteFrame {
    /// Local east, inertial.
    pub east: DVec3,
    /// Local north, inertial.
    pub north: DVec3,
    /// Local up at the site anchor (unit radial), inertial.
    pub up: DVec3,
    /// World-space ground point of the site anchor.
    pub origin: DVec3,
}

/// Resolve the site's inertial ENU frame at the staged spin phase —
/// the same `R(axis, ω·t)` convention `csp_sim::bubble`'s surface-latch
/// record restores poses with (`env.spin_phase` IS `ω·t`), over the
/// `surface_upright` fixed-frame level basis. `origin` sits at the
/// datum altitude + the sampled terrain height, i.e. ON the rendered
/// ground.
pub(crate) fn site_frame(env: &FlightEnv, site: &SiteEntry, ground_r: f64) -> SiteFrame {
    let (sl, cl) = site.latitude_rad.sin_cos();
    let (so, co) = site.longitude_rad.sin_cos();
    let dir_fixed = DVec3::new(cl * co, cl * so, sl);
    let phase = env.spin_phase.rem_euclid(2.0 * core::f64::consts::PI);
    let spin = if phase != 0.0 {
        DQuat::from_axis_angle(env.spin_axis, phase)
    } else {
        DQuat::IDENTITY
    };
    let upright = csp_sim::bubble::surface_upright(site.latitude_rad, site.longitude_rad);
    let q = (spin * upright).normalize();
    let up = spin.rotate(dir_fixed);
    SiteFrame {
        east: q.rotate(DVec3::X),
        north: q.rotate(DVec3::Y),
        up,
        origin: env.body_center + up * ground_r,
    }
}

/// A structure's ground anchor: the tangent-plane offset `(dx, dy)`
/// from the site, re-radialized — the world point under the
/// structure's own footprint centre with its own plumb `up`, so
/// buildings stand vertical on the sphere rather than tilting with
/// the site's horizon. `ground` resolves the rendered terrain radius
/// along a unit inertial direction.
fn ground_anchor(
    f: &SiteFrame,
    body_center: DVec3,
    dx: f64,
    dy: f64,
    ground: &mut dyn FnMut(DVec3) -> f64,
) -> (DVec3, DVec3) {
    let p = f.origin + f.east * dx + f.north * dy;
    let rel = p - body_center;
    let n = rel.norm();
    if n <= 1.0e-9 {
        return (f.origin, f.up);
    }
    let dir = rel / n;
    (body_center + dir * ground(dir), dir)
}

/// Per-structure material terms — `[spec strength, spec exponent, …]`
/// with the baked occlusion left to the per-vertex AO ramp in
/// [`emit_prims`]. Concrete stays matte; steel takes a sheen; the
/// white paint sits between.
const fn mat(spec: f32, exp: f32) -> [f32; 3] {
    [spec, exp, 1.0]
}

/// The site's structure palette — linear albedos matching the part
/// livery's convention (the flight shader treats `color` as linear).
const APRON: [f32; 4] = [0.36, 0.35, 0.33, 1.0];
const MOUNT: [f32; 4] = [0.33, 0.34, 0.37, 1.0];
const SCORCH: [f32; 4] = [0.07, 0.065, 0.06, 1.0];
const STEEL: [f32; 4] = [0.30, 0.33, 0.38, 1.0];
const RUST: [f32; 4] = [0.52, 0.26, 0.09, 1.0];
const PAINT_WHITE: [f32; 4] = [0.72, 0.72, 0.70, 1.0];
const PAINT_RED: [f32; 4] = [0.62, 0.09, 0.06, 1.0];
const VAB_WALL: [f32; 4] = [0.62, 0.61, 0.56, 1.0];
const VAB_ROOF: [f32; 4] = [0.22, 0.23, 0.25, 1.0];
const DOOR: [f32; 4] = [0.14, 0.15, 0.17, 1.0];
const ANNEX: [f32; 4] = [0.52, 0.51, 0.47, 1.0];
const ROAD: [f32; 4] = [0.16, 0.15, 0.14, 1.0];
const SHED_WALL: [f32; 4] = [0.48, 0.45, 0.39, 1.0];
const SHADOW: [f32; 4] = [0.0, 0.0, 0.0, 0.34];

/// One primitive in structure-local ENU coordinates (the structure's
/// own ground anchor is its origin; +z up, +x east, +y north).
enum Prim {
    /// Axis-aligned box — `lo`/`hi` corners.
    Bbox {
        /// Min corner, structure-local metres.
        lo: DVec3,
        /// Max corner, structure-local metres.
        hi: DVec3,
    },
    /// Vertical cylinder / tank — a lathe about the local z axis,
    /// `(z, radius)` stations (see `part_mesh::PartShape::Lathe`).
    Lathe {
        /// Axis xy offset.
        axis: [f64; 2],
        /// Ascending-z `(z, r)` stations.
        profile: &'static [(f64, f64)],
    },
}

/// One piece of the complex: a paint, a material, and its primitives
/// relative to the piece's own ground anchor at `at` (site-frame
/// east/north metres). `foot_m` is the footprint half-size used both
/// for the terrain-height ring sample and the contact-shadow quad.
struct Piece {
    /// Site-frame `(east, north)` offset of the piece's anchor.
    at: (f64, f64),
    /// Footprint half-extent (m) for the ground ring + shadow quad.
    foot_m: (f64, f64),
    /// `false` skips the contact-shadow quad (roads are flat anyway).
    shadow: bool,
    /// Linear albedo.
    color: [f32; 4],
    /// `[spec, exponent]`; baked occlusion comes from the AO ramp.
    mat: [f32; 3],
    /// The geometry.
    prims: &'static [Prim],
}

/// Concrete/apron material — matte.
const MAT_CONCRETE: [f32; 3] = mat(0.06, 10.0);
/// Painted steel — a modest sheen.
const MAT_STEEL: [f32; 3] = mat(0.42, 36.0);
/// Painted surfaces (VAB walls, tank farm, buildings).
const MAT_PAINT: [f32; 3] = mat(0.22, 26.0);

/// The pad's flame-trench fuel-farm tank profile: a skirted vertical
/// cylinder with a shallow dome — the LOX/kerosene ball silhouette.
static TANK_PROFILE: &[(f64, f64)] =
    &[(-0.6, 2.3), (0.0, 2.5), (5.6, 2.5), (6.6, 1.9), (7.3, 0.02)];

/// The §3.4 test stand's site anchor (east, north metres) — the
/// FACILITIES panel's "test stand @ {site}" row needs a rendered
/// counterpart at the launch site (C20-F3). Northeast of the pad:
/// the gantry complex ends at x ≈ 20 and the propellant farm /
/// support sheds hold the south strip, so this quadrant is clear.
/// Named for the layout test, which keys on the constant rather
/// than re-deriving the coordinates.
pub(crate) const TEST_STAND_AT: (f64, f64) = (30.0, 14.0);

/// The whole space-center layout, site-local ENU metres. The pad sits
/// ON the origin — the craft spawns at the site itself, resting at
/// terrain level in the flame-trench slot between the two mount
/// pedestals (the "flush pad": the mount rises around it, no spawn
/// altitude change — the sim's contact surface already IS this
/// ground).
static SPACE_CENTER: &[Piece] = &[
    // ---- Launch mount: two deck pedestals straddling the trench ---
    Piece {
        at: (0.0, 0.0),
        foot_m: (17.0, 15.0),
        shadow: true,
        color: APRON,
        mat: MAT_CONCRETE,
        prims: &[
            // Apron ring around the trench opening (hole x ±3.5, y ±10).
            Prim::Bbox {
                lo: DVec3::new(-15.0, -13.0, -0.55),
                hi: DVec3::new(-3.5, 13.0, 0.12),
            },
            Prim::Bbox {
                lo: DVec3::new(3.5, -13.0, -0.55),
                hi: DVec3::new(15.0, 13.0, 0.12),
            },
            Prim::Bbox {
                lo: DVec3::new(-3.5, -13.0, -0.55),
                hi: DVec3::new(3.5, -10.0, 0.12),
            },
            Prim::Bbox {
                lo: DVec3::new(-3.5, 10.0, -0.55),
                hi: DVec3::new(3.5, 13.0, 0.12),
            },
        ],
    },
    Piece {
        at: (0.0, 0.0),
        foot_m: (11.0, 10.0),
        shadow: false,
        color: MOUNT,
        mat: MAT_CONCRETE,
        prims: &[
            // The mount pedestals: the craft rests in the slot between
            // them at terrain level — deck +2.1 m, flame gap x ±3.
            Prim::Bbox {
                lo: DVec3::new(-10.0, -9.0, -0.6),
                hi: DVec3::new(-3.0, 9.0, 2.1),
            },
            Prim::Bbox {
                lo: DVec3::new(3.0, -9.0, -0.6),
                hi: DVec3::new(10.0, 9.0, 2.1),
            },
        ],
    },
    Piece {
        at: (0.0, 0.0),
        foot_m: (4.0, 10.0),
        shadow: false,
        color: SCORCH,
        mat: MAT_CONCRETE,
        prims: &[
            // Scorched trench liners — blast-darkened inner walls.
            Prim::Bbox {
                lo: DVec3::new(-3.05, -9.0, -0.55),
                hi: DVec3::new(-2.99, 9.0, 2.05),
            },
            Prim::Bbox {
                lo: DVec3::new(2.99, -9.0, -0.55),
                hi: DVec3::new(3.05, 9.0, 2.05),
            },
        ],
    },
    // ---- Service gantry east of the pad ---------------------------
    Piece {
        at: (14.5, 2.0),
        foot_m: (4.5, 4.5),
        shadow: true,
        color: STEEL,
        mat: MAT_STEEL,
        prims: &[
            Prim::Bbox {
                lo: DVec3::new(-1.9, -1.9, -0.5),
                hi: DVec3::new(1.9, 1.9, 0.8),
            },
            Prim::Bbox {
                lo: DVec3::new(-0.9, -0.9, 0.8),
                hi: DVec3::new(0.9, 0.9, 26.0),
            },
        ],
    },
    Piece {
        at: (14.5, 2.0),
        foot_m: (12.0, 3.0),
        shadow: false,
        color: RUST,
        mat: MAT_STEEL,
        prims: &[
            // Deck levels up the column.
            Prim::Bbox {
                lo: DVec3::new(-1.4, -1.4, 4.9),
                hi: DVec3::new(1.4, 1.4, 5.15),
            },
            Prim::Bbox {
                lo: DVec3::new(-1.4, -1.4, 9.9),
                hi: DVec3::new(1.4, 1.4, 10.15),
            },
            Prim::Bbox {
                lo: DVec3::new(-1.4, -1.4, 14.9),
                hi: DVec3::new(1.4, 1.4, 15.15),
            },
            Prim::Bbox {
                lo: DVec3::new(-1.4, -1.4, 19.9),
                hi: DVec3::new(1.4, 1.4, 20.15),
            },
            Prim::Bbox {
                lo: DVec3::new(-1.4, -1.4, 24.9),
                hi: DVec3::new(1.4, 1.4, 25.15),
            },
            // Service arms reaching west to the mount slot.
            Prim::Bbox {
                lo: DVec3::new(-11.6, -0.8, 6.6),
                hi: DVec3::new(-0.9, 0.8, 7.5),
            },
            Prim::Bbox {
                lo: DVec3::new(-11.6, -0.8, 14.6),
                hi: DVec3::new(-0.9, 0.8, 15.5),
            },
            Prim::Bbox {
                lo: DVec3::new(-11.6, -0.8, 22.6),
                hi: DVec3::new(-0.9, 0.8, 23.5),
            },
        ],
    },
    Piece {
        at: (14.5, 2.0),
        foot_m: (1.0, 1.0),
        shadow: false,
        color: PAINT_WHITE,
        mat: MAT_PAINT,
        prims: &[Prim::Bbox {
            lo: DVec3::new(-0.18, -0.18, 26.0),
            hi: DVec3::new(0.18, 0.18, 31.0),
        }],
    },
    Piece {
        at: (14.5, 2.0),
        foot_m: (1.0, 1.0),
        shadow: false,
        color: PAINT_RED,
        mat: MAT_PAINT,
        prims: &[Prim::Bbox {
            lo: DVec3::new(-0.3, -0.3, 31.0),
            hi: DVec3::new(0.3, 0.3, 31.8),
        }],
    },
    // ---- Crawlerway: L of dark roadbed pad → VAB door -------------
    Piece {
        at: (-21.0, 42.0),
        foot_m: (4.0, 30.0),
        shadow: false,
        color: ROAD,
        mat: MAT_CONCRETE,
        prims: &[Prim::Bbox {
            lo: DVec3::new(-3.0, -29.0, -0.45),
            hi: DVec3::new(3.0, 29.0, 0.15),
        }],
    },
    Piece {
        at: (-31.5, 71.0),
        foot_m: (12.0, 4.5),
        shadow: false,
        color: ROAD,
        mat: MAT_CONCRETE,
        prims: &[Prim::Bbox {
            lo: DVec3::new(-10.5, -3.5, -0.45),
            hi: DVec3::new(10.5, 3.5, 0.15),
        }],
    },
    // ---- VAB + west annex -----------------------------------------
    Piece {
        at: (-42.0, 85.0),
        foot_m: (20.0, 16.0),
        shadow: true,
        color: VAB_WALL,
        mat: MAT_PAINT,
        prims: &[Prim::Bbox {
            lo: DVec3::new(-18.0, -14.0, -0.8),
            hi: DVec3::new(18.0, 14.0, 39.0),
        }],
    },
    Piece {
        at: (-42.0, 85.0),
        foot_m: (20.0, 16.0),
        shadow: false,
        color: VAB_ROOF,
        mat: MAT_PAINT,
        prims: &[Prim::Bbox {
            // Roof cap band.
            lo: DVec3::new(-18.4, -14.4, 37.4),
            hi: DVec3::new(18.4, 14.4, 39.6),
        }],
    },
    Piece {
        at: (-42.0, 85.0),
        foot_m: (8.0, 1.0),
        shadow: false,
        color: DOOR,
        mat: MAT_PAINT,
        prims: &[Prim::Bbox {
            // The crawler door — a tall inset panel on the south face.
            lo: DVec3::new(-7.0, -14.2, -0.2),
            hi: DVec3::new(7.0, -14.05, 28.0),
        }],
    },
    Piece {
        at: (-68.0, 85.0),
        foot_m: (9.0, 8.0),
        shadow: true,
        color: ANNEX,
        mat: MAT_PAINT,
        prims: &[Prim::Bbox {
            lo: DVec3::new(-8.0, -7.0, -0.6),
            hi: DVec3::new(8.0, 7.0, 15.0),
        }],
    },
    // ---- Propellant farm southeast --------------------------------
    Piece {
        at: (30.0, -24.0),
        foot_m: (4.5, 4.5),
        shadow: true,
        color: PAINT_WHITE,
        mat: MAT_PAINT,
        prims: &[
            Prim::Bbox {
                lo: DVec3::new(-3.0, -3.0, -0.5),
                hi: DVec3::new(3.0, 3.0, 0.4),
            },
            Prim::Lathe {
                axis: [0.0, 0.0],
                profile: TANK_PROFILE,
            },
        ],
    },
    Piece {
        at: (39.0, -24.0),
        foot_m: (4.5, 4.5),
        shadow: true,
        color: PAINT_WHITE,
        mat: MAT_PAINT,
        prims: &[
            Prim::Bbox {
                lo: DVec3::new(-3.0, -3.0, -0.5),
                hi: DVec3::new(3.0, 3.0, 0.4),
            },
            Prim::Lathe {
                axis: [0.0, 0.0],
                profile: TANK_PROFILE,
            },
        ],
    },
    // ---- Test stand northeast — the §3.4 bench the FACILITIES
    // panel's `RunBenchTest` buttons claim (C20-F3): a concrete
    // apron, the firing pedestal split around its flame channel,
    // the scorch plate under it, and the steel hold-down frame the
    // bench article clamps into. ----
    Piece {
        at: TEST_STAND_AT,
        foot_m: (7.0, 6.0),
        shadow: true,
        color: APRON,
        mat: MAT_CONCRETE,
        prims: &[Prim::Bbox {
            lo: DVec3::new(-6.5, -5.5, -0.45),
            hi: DVec3::new(6.5, 5.5, 0.12),
        }],
    },
    Piece {
        at: TEST_STAND_AT,
        foot_m: (2.5, 2.5),
        shadow: false,
        color: MOUNT,
        mat: MAT_CONCRETE,
        prims: &[
            // Firing pedestal halves — the bench article hangs
            // nozzle-down over the channel between them.
            Prim::Bbox {
                lo: DVec3::new(-2.2, -2.2, -0.1),
                hi: DVec3::new(-0.45, 2.2, 1.7),
            },
            Prim::Bbox {
                lo: DVec3::new(0.45, -2.2, -0.1),
                hi: DVec3::new(2.2, 2.2, 1.7),
            },
        ],
    },
    Piece {
        at: TEST_STAND_AT,
        foot_m: (1.0, 2.5),
        shadow: false,
        color: SCORCH,
        mat: MAT_CONCRETE,
        prims: &[Prim::Bbox {
            // The flame channel's scorched floor between the
            // pedestal halves.
            lo: DVec3::new(-0.45, -2.2, -0.1),
            hi: DVec3::new(0.45, 2.2, 0.25),
        }],
    },
    Piece {
        at: TEST_STAND_AT,
        foot_m: (2.6, 2.6),
        shadow: false,
        color: STEEL,
        mat: MAT_STEEL,
        prims: &[
            // Hold-down frame: four corner posts rising out of the
            // pedestal, capped by the crown ring the article mounts
            // under.
            Prim::Bbox {
                lo: DVec3::new(-2.0, -2.0, 0.0),
                hi: DVec3::new(-1.55, -1.55, 6.4),
            },
            Prim::Bbox {
                lo: DVec3::new(1.55, -2.0, 0.0),
                hi: DVec3::new(2.0, -1.55, 6.4),
            },
            Prim::Bbox {
                lo: DVec3::new(-2.0, 1.55, 0.0),
                hi: DVec3::new(-1.55, 2.0, 6.4),
            },
            Prim::Bbox {
                lo: DVec3::new(1.55, 1.55, 0.0),
                hi: DVec3::new(2.0, 2.0, 6.4),
            },
            Prim::Bbox {
                lo: DVec3::new(-2.15, -2.15, 6.4),
                hi: DVec3::new(2.15, 2.15, 7.0),
            },
        ],
    },
    // ---- Support buildings ----------------------------------------
    Piece {
        at: (-26.0, -30.0),
        foot_m: (7.0, 5.0),
        shadow: true,
        color: SHED_WALL,
        mat: MAT_PAINT,
        prims: &[
            Prim::Bbox {
                lo: DVec3::new(-5.5, -3.5, -0.5),
                hi: DVec3::new(5.5, 3.5, 4.4),
            },
            Prim::Bbox {
                lo: DVec3::new(-5.9, -3.9, 4.4),
                hi: DVec3::new(5.9, 3.9, 4.8),
            },
        ],
    },
    Piece {
        at: (24.0, -32.0),
        foot_m: (4.5, 3.5),
        shadow: true,
        color: SHED_WALL,
        mat: MAT_PAINT,
        prims: &[
            Prim::Bbox {
                lo: DVec3::new(-3.0, -2.5, -0.5),
                hi: DVec3::new(3.0, 2.5, 3.4),
            },
            Prim::Bbox {
                lo: DVec3::new(-3.3, -2.8, 3.4),
                hi: DVec3::new(3.3, 2.8, 3.8),
            },
        ],
    },
];

/// Mottle grid (m): flat faces tessellate to roughly this cell size so
/// the per-vertex albedo jitter has the density to read as
/// concrete/metal surface variation rather than a corner ramp.
const MOTTLE_CELL_M: f64 = 4.0;
/// Midpoint-subdivision cap — 2^4 edge splits bounds a 40 m VAB wall
/// to ~2.5 m cells (256 tris per source tri worst case).
const MOTTLE_MAX_DEPTH: i32 = 4;

/// Deterministic `[0,1)` hash of a structure-local position + prim
/// seed — the albedo-mottle source. Positions quantize to ~1 mm so
/// vertices coincident across a shared face edge jitter identically
/// (no brightness crack at the seam), and the same building always
/// mottles the same way.
fn mottle01(p: DVec3, seed: usize) -> f64 {
    let q = |v: f64| (v * 1024.0).round() as i64 as u64;
    let mut x = q(p.x)
        .wrapping_mul(0xA24B_AED4_963E_E407)
        .wrapping_add(q(p.y).wrapping_mul(0x9FB2_1C65_1E98_DF25))
        .wrapping_add(q(p.z).wrapping_mul(0xC2B2_AE3D_27D4_EB4F))
        .wrapping_add((seed as u64).wrapping_mul(0x1656_67B1_9E37_79F9))
        .wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x >> 11) as f64 * (1.0 / 9.007_199_254_740_992e15)
}

/// `color` with the mottle luminance applied — a ±15% albedo jitter:
/// strong enough that large faces read as poured concrete / painted
/// panels at typical viewing distance, quiet enough not to smudge.
fn mottle_tint(color: [f32; 4], p: DVec3, seed: usize) -> [f32; 4] {
    let lum = (0.85 + 0.30 * mottle01(p, seed)) as f32;
    [color[0] * lum, color[1] * lum, color[2] * lum, color[3]]
}

/// Midpoint-split `t` until its longest edge is ~[`MOTTLE_CELL_M`]
/// (bounded by [`MOTTLE_MAX_DEPTH`]) — large flat faces gain the
/// vertex density the albedo mottle needs. Structure boxes shade
/// flat, so the shared normal carries to every subtri; the lathe
/// prims already tessellate densely enough to skip this.
fn tessellate_face(t: SmoothTri, out: &mut Vec<SmoothTri>) {
    let edge = (t.a - t.b)
        .norm()
        .max((t.b - t.c).norm())
        .max((t.c - t.a).norm());
    let depth = ((edge / MOTTLE_CELL_M).log2().ceil() as i32).clamp(0, MOTTLE_MAX_DEPTH);
    if depth <= 0 {
        out.push(t);
        return;
    }
    let mut stack: Vec<(SmoothTri, i32)> = Vec::with_capacity(1usize << (2 * depth));
    stack.push((t, 0));
    while let Some((cur, d)) = stack.pop() {
        if d >= depth {
            out.push(cur);
            continue;
        }
        let ab = (cur.a + cur.b) * 0.5;
        let bc = (cur.b + cur.c) * 0.5;
        let ca = (cur.c + cur.a) * 0.5;
        let n = [cur.na, cur.nb, cur.nc];
        for (a, b, c) in [
            (cur.a, ab, ca),
            (ab, cur.b, bc),
            (ca, bc, cur.c),
            (ab, bc, ca),
        ] {
            stack.push((
                SmoothTri {
                    a,
                    b,
                    c,
                    na: n[0],
                    nb: n[1],
                    nc: n[2],
                },
                d + 1,
            ));
        }
    }
}

/// Emit one triangle's lit vertices — the all-or-nothing projection
/// rule `extract_flight`'s part pass uses (a corner behind the eye
/// drops the whole tri). `world` carries the camera-relative position;
/// `occ` is baked into `mat[2]` per vertex by the caller, and `col`
/// carries the per-vertex mottle-tinted albedo.
#[allow(clippy::too_many_arguments)]
fn lit_tri(
    v: &mut Vec<csp_render::flight::FlightVertex>,
    vp: &[[f64; 4]; 4],
    eye: DVec3,
    w: [DVec3; 3],
    n: [DVec3; 3],
    col: [[f32; 4]; 3],
    spec_exp: [f32; 3],
    occ: [f32; 3],
) {
    let [Some(c0), Some(c1), Some(c2)] = w.map(|q| project_into(vp, eye, q)) else {
        return;
    };
    for k in 0..3 {
        v.push(csp_render::flight::FlightVertex {
            pos: [c0, c1, c2][k],
            world: [
                (w[k].x - eye.x) as f32,
                (w[k].y - eye.y) as f32,
                (w[k].z - eye.z) as f32,
            ],
            nrm: [n[k].x as f32, n[k].y as f32, n[k].z as f32],
            color: col[k],
            mat: [spec_exp[0], spec_exp[1], occ[k]],
        });
    }
}

/// Emit a piece's primitives transformed by the ENU basis at
/// `anchor`/`up`. Per-vertex baked occlusion: a 0.55→1.0 ramp over the
/// first 3 m above the prim's own base, so wall bottoms read grounded
/// against the contact-shadow quads the pieces sit on. A second
/// per-vertex term — the [`mottle01`] albedo jitter over the
/// [`tessellate_face`]d faces — keeps the large walls and pads from
/// reading as flat untextured quads.
#[allow(clippy::too_many_arguments)] // ENU basis + paint + scratch — one emit call
fn emit_prims(
    v: &mut Vec<csp_render::flight::FlightVertex>,
    vp: &[[f64; 4]; 4],
    eye: DVec3,
    anchor: DVec3,
    east: DVec3,
    north: DVec3,
    up: DVec3,
    color: [f32; 4],
    mat: [f32; 3],
    prims: &[Prim],
    tris: &mut Vec<SmoothTri>,
) {
    let to_world = |p: DVec3| anchor + east * p.x + north * p.y + up * p.z;
    let n_world = |n: DVec3| east * n.x + north * n.y + up * n.z;
    let occ = |z: f64| (0.55 + 0.45 * (z / 3.0).clamp(0.0, 1.0)) as f32;
    // Second scratch for the mottle tessellation — `tris` stays the
    // emit_smooth target the caller allocated.
    let mut tess: Vec<SmoothTri> = Vec::new();
    for (pi, prim) in prims.iter().enumerate() {
        tris.clear();
        match prim {
            Prim::Bbox { lo, hi } => {
                crate::part_mesh::emit_smooth(
                    &crate::part_mesh::PartShape::Box { lo: *lo, hi: *hi },
                    tris,
                );
                // Flat faces retessellate to the mottle grid so the
                // per-vertex albedo jitter reads as concrete/metal
                // surface variation, not a corner-to-corner ramp.
                tess.clear();
                for t in tris.iter() {
                    tessellate_face(*t, &mut tess);
                }
                std::mem::swap(tris, &mut tess);
            }
            Prim::Lathe { axis, profile } => {
                crate::part_mesh::emit_smooth(
                    &crate::part_mesh::PartShape::Lathe {
                        axis: *axis,
                        profile: profile.to_vec(),
                    },
                    tris,
                );
            }
        }
        for t in tris.iter() {
            let (wa, na) = (to_world(t.a), n_world(t.na));
            let (wb, nb) = (to_world(t.b), n_world(t.nb));
            let (wc, nc) = (to_world(t.c), n_world(t.nc));
            lit_tri(
                v,
                vp,
                eye,
                [wa, wb, wc],
                [na, nb, nc],
                [
                    mottle_tint(color, t.a, pi),
                    mottle_tint(color, t.b, pi),
                    mottle_tint(color, t.c, pi),
                ],
                mat,
                [occ(t.a.z), occ(t.b.z), occ(t.c.z)],
            );
        }
    }
}

/// An unlit ground quad — the contact shadow: a flat dark sheet a few
/// centimetres above the sampled terrain max under the piece. The
/// depth-tested lit geometry stands on it; at the pad's flat site it
/// reads as the structure's own shadowing on the apron.
#[allow(clippy::too_many_arguments)]
fn shadow_quad(
    v: &mut Vec<csp_render::flight::FlightVertex>,
    vp: &[[f64; 4]; 4],
    eye: DVec3,
    anchor: DVec3,
    east: DVec3,
    north: DVec3,
    up: DVec3,
    half: (f64, f64),
    color: [f32; 4],
) {
    let c = anchor + up * 0.05;
    let p = [
        c - east * half.0 - north * half.1,
        c + east * half.0 - north * half.1,
        c + east * half.0 + north * half.1,
        c - east * half.0 + north * half.1,
    ];
    for (a, b, cc) in [(p[0], p[1], p[2]), (p[0], p[2], p[3])] {
        let [Some(c0), Some(c1), Some(c2)] = [a, b, cc].map(|q| project_into(vp, eye, q)) else {
            continue;
        };
        for pos in [c0, c1, c2] {
            v.push(csp_render::flight::FlightVertex {
                pos,
                world: [0.0; 3],
                nrm: [0.0; 3],
                color,
                mat: [0.0; 3],
            });
        }
    }
}

/// Emit the launch-site space center into the flight vertex stream:
/// every [`Piece`] of [`SPACE_CENTER`] anchored to the rendered
/// terrain under its own footprint (max over a footprint ring — the
/// same heightfield sample `surface_floor_radius` makes), the
/// unlit contact-shadow quads under each standing structure, then the
/// lit geometry sharing the frame's sun/hemisphere lighting via
/// `packet.light`.
///
/// Called from `extract_flight` only when the terrain pass is live —
/// the structures anchor to the same surface it draws.
pub(crate) fn emit_launch_complex(
    v: &mut Vec<csp_render::flight::FlightVertex>,
    vp: &[[f64; 4]; 4],
    eye: DVec3,
    env: &FlightEnv,
    site: &SiteEntry,
    terrain_cfg: &TerrainConfig,
    terrain_cache: &mut TerrainCache,
) {
    // The site's own ground: the rendered surface under the site
    // direction (footprint ~30 m — a wide ring sample so the complex
    // never hangs over an un-sampled dip).
    let mut ground = |dir: DVec3, footprint_m: f64| {
        crate::terrain::ground_radius(
            terrain_cfg,
            terrain_cache,
            env,
            dir,
            footprint_m / env.radius_m.max(1.0),
        )
    };
    let (sl, cl) = site.latitude_rad.sin_cos();
    let (so, co) = site.longitude_rad.sin_cos();
    let dir_fixed = DVec3::new(cl * co, cl * so, sl);
    let phase = env.spin_phase.rem_euclid(2.0 * core::f64::consts::PI);
    let dir_i = if phase != 0.0 {
        DQuat::from_axis_angle(env.spin_axis, phase).rotate(dir_fixed)
    } else {
        dir_fixed
    };
    let site_ground = ground(dir_i, 30.0);
    let frame = site_frame(env, site, site_ground);

    let mut tris = Vec::new();
    for piece in SPACE_CENTER {
        let foot = piece.foot_m.0.hypot(piece.foot_m.1);
        let (anchor, up) = {
            let mut g = |d: DVec3| ground(d, foot);
            ground_anchor(&frame, env.body_center, piece.at.0, piece.at.1, &mut g)
        };
        if piece.shadow {
            shadow_quad(
                v,
                vp,
                eye,
                anchor,
                frame.east,
                frame.north,
                up,
                piece.foot_m,
                SHADOW,
            );
        }
        emit_prims(
            v,
            vp,
            eye,
            anchor,
            frame.east,
            frame.north,
            up,
            piece.color,
            piece.mat,
            piece.prims,
            &mut tris,
        );
    }
}

/// The launch site's inertial surface position — the map marker's
/// anchor (system frame: `body_pos` plus the body-relative radial).
/// `body_pos` is the site's body position in the system frame; the
/// radial uses the staged dominant-body spin (`env`) — markers are
/// only solved for sites on the dominant body, the only body whose
/// rotation `FlightEnv` carries.
pub(crate) fn site_world_pos(env: &FlightEnv, site: &SiteEntry, body_pos: DVec3) -> DVec3 {
    let (sl, cl) = site.latitude_rad.sin_cos();
    let (so, co) = site.longitude_rad.sin_cos();
    let dir_fixed = DVec3::new(cl * co, cl * so, sl) * (env.radius_m + site.altitude_m);
    let phase = env.spin_phase.rem_euclid(2.0 * core::f64::consts::PI);
    let rel = if phase != 0.0 {
        DQuat::from_axis_angle(env.spin_axis, phase).rotate(dir_fixed)
    } else {
        dir_fixed
    };
    body_pos + rel
}

#[cfg(test)]
// Tests signal failure by panicking (§2.13 covers shipping paths).
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// C20-F3: the FACILITIES panel's "test stand @ {site}" row has a
    /// rendered counterpart at the launch site — the layout must
    /// still carry a recognizable firing stand at `TEST_STAND_AT`:
    /// several pieces (apron + pedestal + scorch + frame) and at
    /// least one prim reaching stand height. A silent deletion of
    /// the pieces fails here, not in a screenshot.
    #[test]
    fn the_test_stand_is_rendered_at_the_launch_site() {
        let stand: Vec<&Piece> = SPACE_CENTER
            .iter()
            .filter(|p| p.at == TEST_STAND_AT)
            .collect();
        assert!(
            stand.len() >= 3,
            "the stand needs its apron, pedestal, and frame pieces (got {})",
            stand.len()
        );
        let tallest = stand
            .iter()
            .flat_map(|p| p.prims.iter())
            .fold(0.0_f64, |m, prim| {
                m.max(match prim {
                    Prim::Bbox { hi, .. } => hi.z,
                    Prim::Lathe { profile, .. } => profile.iter().fold(0.0_f64, |z, s| z.max(s.0)),
                })
            });
        assert!(
            tallest >= 5.0,
            "the hold-down frame must reach ≥5 m to read as a stand (got {tallest})"
        );
        // The stand anchors clear of the pad apron (the origin block
        // spans ±15 m east / ±13 m north) — no piece may intrude on
        // the spawn volume.
        assert!(TEST_STAND_AT.0 - 7.0 > 15.0, "stand apron overlaps the pad");
    }
}
