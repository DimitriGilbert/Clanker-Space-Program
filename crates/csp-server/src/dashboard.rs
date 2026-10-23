//! M9d §2.12.2 — the sidecar dashboard config.
//!
//! "**Dashboards are non-authoritative sidecar config, not save
//! data.** They live in `$config_dir/csp/dashboards/*.json`,
//! versioned independently (`schema_version`), **never in the save,
//! never in the command log, never in the state hash**." The store
//! here is the honest implementation of that rule: a plain
//! `*.json`-per-dashboard directory, a versioned schema, caps
//! everywhere, and no path at all from a dashboard into `World`
//! state — the only mutation channel is this module (the HTTP
//! `/api/dashboards` routes and the session-class `dashboard.*` MCP
//! tools, which are *not replay-relevant* by declaration).
//!
//! # Schema (v1)
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "name": "flight",
//!   "title": "Flight",
//!   "rate_hz": 16,
//!   "widgets": [{
//!     "id": "alt-gauge", "kind": "gauge", "title": "Altitude",
//!     "fields": ["vessels.0.altitude_m"],
//!     "rect": {"x": 0, "y": 0, "w": 4, "h": 3},
//!     "options": {"unit": "m", "max": 120000}
//!   }]
//! }
//! ```
//!
//! `fields` are the §2.12.2 subscription paths — a dashboard's union
//! must fit one subscription ([`MAX_FIELDS`], the WS cap), so a
//! stored dashboard is always subscribable as-is. `kind` is the
//! widget registry's key set ([`WIDGET_KINDS`]) — the server's
//! mirror of the SPA's `apps/telemetry` registry; unknown kinds
//! refuse at validation, not at render.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The dashboard schema version — bumped deliberately; old files
/// refuse rather than silently misparse.
pub const DASHBOARD_SCHEMA_VERSION: u32 = 1;
/// Dashboard count bound — a config dir is curated, not a database.
pub const MAX_DASHBOARDS: usize = 64;
/// Widgets per dashboard — the layout bound.
pub const MAX_WIDGETS: usize = 24;
/// Field paths per widget — a widget is a bounded subscription slice.
pub const MAX_WIDGET_FIELDS: usize = 16;
/// Name bound (`[A-Za-z0-9._-]` — the `safe_name` rule).
pub const MAX_NAME: usize = 64;
/// The widget registry's kind set — server-side mirror of
/// `apps/telemetry/src/lib/widgets/registry.tsx`. Additive-only.
pub const WIDGET_KINDS: &[&str] = &["gauge", "graph", "table", "orbit-map"];

/// The §2.12.2 rate rule, repeated for dashboards: divisors of 64,
/// default 16. Same set `http.rs` validates subscriptions against.
const VALID_RATES: &[u64] = &[64, 32, 16, 8, 4, 2, 1];
/// The WS subscription cap (`http.rs::MAX_FIELDS`) — a dashboard's
/// *union* of widget fields must fit it.
const MAX_SUBSCRIPTION_FIELDS: usize = 64;

/// One widget — a kind, the field paths it subscribes, its grid
/// rect, and kind-specific options (`unit`, `min`/`max`, `window_s`,
/// columns, … — free-form JSON, validated only for shape).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Widget {
    /// Unique within the dashboard — the layout/state key.
    pub id: String,
    /// The registry kind (`"gauge"`, `"graph"`, `"table"`,
    /// `"orbit-map"` — [`WIDGET_KINDS`]).
    pub kind: String,
    /// The tile label.
    pub title: String,
    /// §2.12.2 subscription paths this widget reads.
    pub fields: Vec<String>,
    /// Grid placement — 12-column units (the SPA's layout contract).
    pub rect: Rect,
    /// Kind-specific options — free-form (`unit`, `min`, `max`,
    /// `window_s`, `columns`, …); the widget interprets them.
    #[serde(default)]
    pub options: serde_json::Map<String, serde_json::Value>,
}

/// Grid rect — column units on a 12-col grid, row units of the
/// SPA's tile height.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    /// Column offset, 0..12.
    pub x: u32,
    /// Row offset.
    pub y: u32,
    /// Column span, ≥1 (≤ 12 enforced at validation).
    pub w: u32,
    /// Row span, ≥1.
    pub h: u32,
}

/// A dashboard — the stored sidecar document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dashboard {
    /// [`DASHBOARD_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The file-stem name — also the MCP/API key.
    pub name: String,
    /// The display title.
    pub title: String,
    /// The subscription rate the dashboard requests — a 64-Hz
    /// divisor, default 16.
    #[serde(default = "default_rate")]
    pub rate_hz: u64,
    /// The layout, stored order = render order.
    pub widgets: Vec<Widget>,
}

fn default_rate() -> u64 {
    16
}

/// The field-path union the dashboard's widgets need — exactly what
/// the SPA puts in its `subscribe` message (deduped, sorted).
pub fn subscription_fields(dash: &Dashboard) -> Vec<String> {
    dash.widgets
        .iter()
        .flat_map(|w| w.fields.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// Store/validation failures — HTTP 400/404/500 map onto these.
#[derive(Debug)]
pub enum DashboardError {
    /// Filesystem failure.
    Io(std::io::Error),
    /// A stored or submitted document is not the schema.
    Json(serde_json::Error),
    /// Validation refusal (bad name, caps, unknown kind, field
    /// union over the subscription cap, schema version).
    Invalid(String),
}

impl fmt::Display for DashboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "dashboard store: {e}"),
            Self::Json(e) => write!(f, "dashboard json: {e}"),
            Self::Invalid(m) => write!(f, "invalid dashboard: {m}"),
        }
    }
}

impl std::error::Error for DashboardError {}

/// The `*.json` sidecar directory — one file per dashboard.
/// Constructed per host (`serve` config / `csp-mcp` flag); `None`
/// in a config-less host keeps the endpoints honest (503, not a
/// silent empty list).
pub struct DashboardStore {
    dir: PathBuf,
}

impl DashboardStore {
    /// The store over `dir` — created on first `save`.
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.json"))
    }

    /// The stored dashboard names — sorted (BTreeMap-discipline for
    /// the API's stable list).
    pub fn list(&self) -> Result<Vec<String>, DashboardError> {
        let mut out = Vec::new();
        let dir = match std::fs::read_dir(&self.dir) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(DashboardError::Io(e)),
        };
        for entry in dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json")
                && let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            {
                out.push(stem.to_string());
            }
        }
        out.sort();
        Ok(out)
    }

    /// Read + validate one dashboard — `Invalid` for a missing file
    /// (404-shaped) or a bad document.
    pub fn load(&self, name: &str) -> Result<Dashboard, DashboardError> {
        check_name(name)?;
        let bytes = std::fs::read(self.path(name)).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                DashboardError::Invalid(format!("no dashboard {name:?}"))
            }
            _ => DashboardError::Io(e),
        })?;
        let dash: Dashboard = serde_json::from_slice(&bytes).map_err(DashboardError::Json)?;
        dash.validate()?;
        Ok(dash)
    }

    /// Validate + write one dashboard — the only mutation path. The
    /// store caps total dashboards (a new name at the cap refuses).
    pub fn save(&self, name: &str, dash: &Dashboard) -> Result<(), DashboardError> {
        check_name(name)?;
        if dash.name != name {
            return Err(DashboardError::Invalid(format!(
                "dashboard name {:?} does not match the store key {name:?}",
                dash.name
            )));
        }
        dash.validate()?;
        if !self.path(name).exists() && self.list()?.len() >= MAX_DASHBOARDS {
            return Err(DashboardError::Invalid(format!(
                "dashboard cap {MAX_DASHBOARDS} reached"
            )));
        }
        std::fs::create_dir_all(&self.dir).map_err(DashboardError::Io)?;
        let bytes = serde_json::to_vec_pretty(dash).map_err(DashboardError::Json)?;
        std::fs::write(self.path(name), bytes).map_err(DashboardError::Io)
    }
}

/// The `safe_name` rule (`backend.rs`'s save-name convention).
fn check_name(name: &str) -> Result<(), DashboardError> {
    let ok = !name.is_empty()
        && name.len() <= MAX_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(DashboardError::Invalid(format!(
            "name must be 1-{MAX_NAME} chars of [A-Za-z0-9._-], got {name:?}"
        )))
    }
}

impl Dashboard {
    /// The stored-document check — version pin, name, caps, known
    /// kinds, rate divisor, rect sanity, and the field union fitting
    /// one subscription. Every refusal is named (a bad dashboard is
    /// an authoring error, not a corrupt file).
    pub fn validate(&self) -> Result<(), DashboardError> {
        if self.schema_version != DASHBOARD_SCHEMA_VERSION {
            return Err(DashboardError::Invalid(format!(
                "schema_version {} != {DASHBOARD_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        check_name(&self.name)?;
        if !VALID_RATES.contains(&self.rate_hz) {
            return Err(DashboardError::Invalid(format!(
                "rate_hz must divide 64: {VALID_RATES:?}"
            )));
        }
        if self.widgets.len() > MAX_WIDGETS {
            return Err(DashboardError::Invalid(format!(
                "widgets {} > {MAX_WIDGETS}",
                self.widgets.len()
            )));
        }
        let mut ids = BTreeSet::new();
        for w in &self.widgets {
            if w.id.is_empty() || w.id.len() > MAX_NAME || !ids.insert(&w.id) {
                return Err(DashboardError::Invalid(format!(
                    "widget id {:?} empty/duplicate/too long",
                    w.id
                )));
            }
            if !WIDGET_KINDS.contains(&w.kind.as_str()) {
                return Err(DashboardError::Invalid(format!(
                    "unknown widget kind {:?} (registry: {WIDGET_KINDS:?})",
                    w.kind
                )));
            }
            if w.fields.is_empty() || w.fields.len() > MAX_WIDGET_FIELDS {
                return Err(DashboardError::Invalid(format!(
                    "widget {:?} fields must be 1-{MAX_WIDGET_FIELDS}",
                    w.id
                )));
            }
            for f in &w.fields {
                if f.is_empty() || f.len() > 128 {
                    return Err(DashboardError::Invalid(format!(
                        "widget {:?} has an empty/over-long field path",
                        w.id
                    )));
                }
            }
            if w.rect.w == 0 || w.rect.h == 0 || w.rect.x + w.rect.w > 12 {
                return Err(DashboardError::Invalid(format!(
                    "widget {:?} rect {}+{} overflows the 12-col grid",
                    w.id, w.rect.x, w.rect.w
                )));
            }
        }
        let union = subscription_fields(self).len();
        if union > MAX_SUBSCRIPTION_FIELDS {
            return Err(DashboardError::Invalid(format!(
                "field union {union} > the {MAX_SUBSCRIPTION_FIELDS}-field subscription cap"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test assertions
mod tests {
    use super::*;
    use serde_json::json;

    fn widget(id: &str, kind: &str, fields: &[&str]) -> Widget {
        Widget {
            id: id.to_string(),
            kind: kind.to_string(),
            title: id.to_string(),
            fields: fields.iter().map(|s| s.to_string()).collect(),
            rect: Rect {
                x: 0,
                y: 0,
                w: 4,
                h: 3,
            },
            options: serde_json::Map::new(),
        }
    }

    fn dash() -> Dashboard {
        Dashboard {
            schema_version: DASHBOARD_SCHEMA_VERSION,
            name: "flight".into(),
            title: "Flight".into(),
            rate_hz: 16,
            widgets: vec![
                widget("a", "gauge", &["vessels.0.altitude_m"]),
                widget("b", "table", &["tick", "warp_level"]),
            ],
        }
    }

    #[test]
    fn store_round_trip() {
        let dir = std::env::temp_dir().join(format!("csp-dash-{}", std::process::id()));
        let store = DashboardStore::new(dir.clone());
        store.save("flight", &dash()).expect("save");
        let loaded = store.load("flight").expect("load");
        assert_eq!(loaded, dash());
        assert_eq!(store.list().unwrap(), vec!["flight".to_string()]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn validation_refusals() {
        let mut d = dash();
        d.schema_version = 2;
        assert!(matches!(d.validate(), Err(DashboardError::Invalid(_))));
        let mut d = dash();
        d.rate_hz = 60; // not a 64-divisor — §2.12.2's documented case
        assert!(matches!(d.validate(), Err(DashboardError::Invalid(_))));
        let mut d = dash();
        d.widgets[0].kind = "hologram".into();
        assert!(matches!(d.validate(), Err(DashboardError::Invalid(_))));
        let mut d = dash();
        d.widgets[1].id = "a".into(); // duplicate id
        assert!(matches!(d.validate(), Err(DashboardError::Invalid(_))));
        let mut d = dash();
        d.widgets[0].rect.x = 10;
        d.widgets[0].rect.w = 4; // 10+4 > 12
        assert!(matches!(d.validate(), Err(DashboardError::Invalid(_))));
    }

    #[test]
    fn field_union_fits_one_subscription() {
        let mut d = dash();
        d.widgets.clear();
        // 5 widgets × 16 fields = an 80-field union — over the
        // WS subscription cap even though every widget is legal.
        for i in 0..5 {
            let fields: Vec<String> = (0..16)
                .map(|j| format!("vessels.0.f{}", i * 16 + j))
                .collect();
            d.widgets.push(Widget {
                id: format!("w{i}"),
                kind: "graph".into(),
                title: "w".into(),
                fields,
                rect: Rect {
                    x: 0,
                    y: 0,
                    w: 4,
                    h: 3,
                },
                options: serde_json::Map::new(),
            });
        }
        assert!(
            matches!(d.validate(), Err(DashboardError::Invalid(_))),
            "field union over the WS cap"
        );
        // 60 fields union → fits the subscription → valid.
        d.widgets.pop();
        assert!(d.validate().is_ok());
    }

    #[test]
    fn subscription_fields_dedupes_and_sorts() {
        let d = dash();
        assert_eq!(
            subscription_fields(&d),
            vec![
                "tick".to_string(),
                "vessels.0.altitude_m".to_string(),
                "warp_level".to_string()
            ]
        );
    }

    #[test]
    fn serde_shape_is_the_documented_json() {
        let v: serde_json::Value = serde_json::from_value(json!({
            "schema_version": 1, "name": "d", "title": "D",
            "rate_hz": 16,
            "widgets": [{"id": "w", "kind": "gauge", "title": "W",
                         "fields": ["tick"], "rect": {"x":0,"y":0,"w":4,"h":3}}],
        }))
        .unwrap();
        let _d: Dashboard = serde_json::from_value(v).unwrap();
        // Unknown keys refuse — a misspelled option is an authoring error.
        let bad = json!({"schema_version": 1, "name": "d", "title": "D",
            "widgets": [], "bogus": 1});
        assert!(serde_json::from_value::<Dashboard>(bad).is_err());
    }
}
