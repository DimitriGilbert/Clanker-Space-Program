//! M8-B §3.5.1 startup wiring: `View::new` must found the home
//! system's authored starting colonies (`data/systems/home/
//! colonies.ron`) at world build — the gate that keeps the file a
//! live input rather than dead data (the csp-data `colonies` gate
//! covers the file's validity; this covers the session actually
//! installing it).

#![cfg(not(target_arch = "wasm32"))]
// Tests signal failure by panicking — §2.13's no-panic rule is for the
// shipping fallible paths, so unwrap/expect are allowed in test code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csp_data::SiteId;
use csp_view::View;

/// `View::new` on the shipping data set starts the session with the
/// authored `rask-mines` colony already founded — modules, stores,
/// and published deadlines in place from tick 0.
#[test]
fn startup_founds_authored_colonies() {
    let view = View::new().unwrap();
    let colonies = view.sim.colonies();
    assert_eq!(
        colonies.len(),
        1,
        "expected the authored rask-mines colony at session start"
    );
    let colony = colonies.values().next().unwrap();
    assert_eq!(colony.site, SiteId::from("rask-mines"));
    assert!(
        !colony.modules.is_empty(),
        "founded colony carries its authored module list"
    );
}
