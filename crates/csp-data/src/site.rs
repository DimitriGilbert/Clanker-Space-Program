//! The `sites.ron` schema — the §3.5 surface-site registry.
//!
//! A site is an authored surface location on a body: where craft
//! launch from and where `SimCommand::RecoverVessel` credits a
//! recovered vessel's resource contents. §3.5's recovery rule —
//! *"credits a vessel's resource contents to the recovery site's
//! inventory at `recovery_efficiency` (default 0.9, 1.0 at the launch
//! site)"* — makes the *efficiency* a tunable (`balance.ron:
//! recovery_efficiency`, with the launch-site override built into the
//! sim), so the site file authors only geography and the `launch`
//! flag: a launch site is the 1.0-efficiency site on its body; every
//! other site is a plain recovery site at the balance default.
//!
//! Sites live at system scope (`data/systems/<sys>/sites.ron`) rather
//! than per-body because they are program geography, not body
//! physics — the body files describe the world, this file describes
//! where the program put its pads.

use serde::{Deserialize, Serialize};

use crate::ids::{BodyId, LocKey, SiteId};

/// The `schema_version` this loader understands for `sites.ron` — its
/// own namespace, like `balance.ron` (§3.1 first-field convention).
pub const SITE_SCHEMA_VERSION: u32 = 1;

/// One authored surface site (§3.5). The geocentric pose uses the
/// same convention as `csp_sim::SurfacePose` (body-fixed latitude,
/// longitude eastward from the fixed frame's +x, altitude above the
/// datum) so the sim can compare a latched vessel's pose to a site's
/// location without a frame dance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SiteDef {
    /// First field of every data file (§3.1). Must equal
    /// [`SITE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Stable site ID (`"terrin-launch"` — §3.1's stable-strings rule).
    pub id: SiteId,
    /// Loc key for the display name.
    pub display_name: LocKey,
    /// The body the site sits on — a `system.bodies` member
    /// (validator rule).
    pub body: BodyId,
    /// Geocentric latitude of the site, rad — `[-π/2, π/2]`.
    pub latitude_rad: f64,
    /// Longitude, rad — eastward from the fixed frame's +x.
    pub longitude_rad: f64,
    /// Altitude above the body's datum radius (m) — non-negative.
    pub altitude_m: f64,
    /// `true` = the body's launch site: `recovery_efficiency` 1.0 per
    /// §3.5 (the sim derives it; the file only flags it). The
    /// validator enforces at most one launch site per body.
    pub launch: bool,
}

/// `data/systems/<sys>/sites.ron` — the system's site set. Optional:
/// a missing file reads as an empty set (the `load_experiments`
/// convention) — a system without sites simply refuses recovery
/// commands, it does not fail to load.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SiteFile {
    /// First field of every data file (§3.1). Must equal
    /// [`SITE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The authored sites. IDs must be unique (validator rule).
    pub sites: Vec<SiteDef>,
}
