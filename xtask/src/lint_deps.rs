//! `cargo xtask lint-deps` — the PLAN §2.1 dependency-rule gate (§8.2 row
//! "Dependency rules"). Parses `cargo tree --format "{p}"` per crate and
//! checks it against explicit positive allowlists and denylists.
//!
//! Design rules inherited from §8.2 and the audit:
//!
//! - **Fail loudly on tooling error.** A failed `cargo tree` spawn or a
//!   non-zero exit is `exit(2)`, never a pass. Empty output is impossible
//!   to pass on: [`parse_tree`] rejects output that does not contain the
//!   linted crate as its depth-0 root.
//! - **Exact package-name equality**, never substring (the old `"rand "`
//!   hack matched nothing, and `rand_chacha`/`rand_core` are the allowed
//!   pair while `rand` is banned). The one exception is `bevy_*`, which is
//!   a *family* — matched as an explicit prefix.
//! - **Edge scope:** trees are fetched with `--edges normal,build`, so
//!   dev-deps are out of scope for both allow and deny. §8.1 names
//!   `proptest` as the property-test layer for the determinism crates and
//!   it legitimately pulls the `rand` facade; code-level bans
//!   (`clippy.toml` `disallowed-methods`/`disallowed-types`, which also
//!   cover `#[cfg(test)]` code) are what keep the facade out of the sim
//!   itself. `std::time` is likewise a *code* ban (clippy.toml), not a
//!   cargo-tree ban — there is no `std::time` crate.
//! - **Testable:** the rule engine [`check_deps`] is a pure function over
//!   parsed trees, unit-tested against fixture `cargo tree` output —
//!   including fixtures that MUST produce violations (the negative test
//!   proving the lint can fail).

use std::process::{Command, exit};

/// A banned package in a dependency tree.
#[derive(Clone, Copy)]
pub enum Ban {
    /// Exact package-name equality.
    Exact(&'static str),
    /// Name prefix — for crate families (`bevy_`), not for single crates.
    Prefix(&'static str),
}

impl Ban {
    fn name_matches(self, name: &str) -> bool {
        match self {
            Ban::Exact(b) => name == b,
            Ban::Prefix(p) => name.starts_with(p),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Ban::Exact(b) => b,
            Ban::Prefix(p) => p,
        }
    }
}

/// The §2.1 determinism domain: no `bevy_*`, `wgpu`, `winit`, `egui`,
/// `rayon`, `tokio`, `getrandom`, or `rand` facade anywhere in the
/// dependency tree. `std::time` is deliberately absent — it is a code-level
/// ban enforced by the crates' `clippy.toml`, not a package.
const DETERMINISM_CRATES: &[&str] = &["csp-core", "csp-data", "csp-sim", "csp-phys"];
const DETERMINISM_BANS: &[Ban] = &[
    Ban::Prefix("bevy_"),
    Ban::Exact("bevy"),
    Ban::Exact("wgpu"),
    Ban::Exact("winit"),
    Ban::Exact("egui"),
    Ban::Exact("rayon"),
    Ban::Exact("tokio"),
    Ban::Exact("getrandom"),
    Ban::Exact("rand"),
];

/// §2.1 positive allowlists — the *direct* (depth-1, non-dev) deps each
/// listed crate may declare. A direct dep outside the set is a violation,
/// which is what makes "may depend only on" enforceable rather than a wish.
/// csp-sim's set is the §2.1 list verbatim; the others are the §2.1-implied
/// minimal surfaces for crates still being built out.
const ALLOW: &[(&str, &[&str])] = &[
    (
        "csp-sim",
        &[
            "csp-core",
            "csp-data",
            "csp-phys",
            "libm",
            "serde",
            "blake3",
            "rand_chacha",
            "rand_core",
        ],
    ),
    // "Units, SimTime, IDs, f64 math (libm only), RNG KDF, StateHasher,
    // error types. Depends on nothing below." Save-schema codecs are the
    // only planned addition beyond today's RNG/hash trio.
    (
        "csp-core",
        &[
            "blake3",
            "rand_chacha",
            "rand_core",
            "libm",
            "serde",
            "bincode",
            "ciborium",
            "miniz_oxide",
        ],
    ),
    // "part/system/tech/balance defs (RON), loader + validator, frozen save
    // schemas + migrations. No sim logic."
    (
        "csp-data",
        &[
            "csp-core",
            "libm",
            "serde",
            "serde_json",
            "ron",
            "bincode",
            "ciborium",
            "miniz_oxide",
            "blake3",
        ],
    ),
    // "weld (pure fn), collision + rigid-body integration behind
    // `PhysicsBackend` (parry3d-f64 + own integrator; rapier for joints)".
    (
        "csp-phys",
        &[
            "csp-core",
            "csp-data",
            "libm",
            "serde",
            "blake3",
            "rand_core",
            "rand_chacha",
            "bincode",
            "parry3d-f64",
            "rapier3d-f64",
        ],
    ),
];

/// Per-crate tree bans beyond the determinism set.
const EXTRA_BANS: &[(&str, &[Ban])] = &[
    // §2.1: "csp-render must not depend on csp-sim. It consumes
    // `csp_render::Frame` built by csp-view" — so csp-view is banned too
    // (the view->render edge is one-way), and no bevy_* belongs in a
    // renderer that is not the ECS crate.
    (
        "csp-render",
        &[
            Ban::Exact("csp-sim"),
            Ban::Exact("csp-view"),
            Ban::Prefix("bevy_"),
            Ban::Exact("bevy"),
        ],
    ),
];

/// Crates whose wasm32 trees get the §8.2 wasm-purity assertion. csp-app is
/// the row's named crate; the rest of the wasm build surface is asserted
/// for the same reason (a banned crate entering via any wasm member
/// poisons the bundle).
const WASM_PKGS: &[&str] = &["csp-app", "csp-view", "csp-render", "csp-sim", "csp-ui"];

/// §8.2 wasm ban list. `tokio` carries the one documented exception —
/// see [`check_deps`]/PLAN §8.2.
const WASM_BANS: &[Ban] = &[
    Ban::Exact("csp-server"),
    Ban::Exact("csp-bench"),
    Ban::Exact("rayon"),
    Ban::Exact("tokio"),
];

/// The only permitted parent edge for `tokio` in a wasm tree: Cargo.lock
/// shows `wasm-bindgen-futures` 0.4.79 declares a `tokio` dependency. It
/// resolves out of the wasm32 tree today, but if a feature flip ever
/// activates it, `tokio` is allowed ONLY through this edge — every other
/// path to `tokio` is a violation.
const TOKIO_ALLOWED_PARENT: &str = "wasm-bindgen-futures";

/// Every workspace member `lint-deps` walks (§2.1's "the dependency lint
/// must still list it" — missing crates simply are not trees yet; a member
/// present in the workspace is linted).
const ALL_PKGS: &[&str] = &[
    "csp-core",
    "csp-data",
    "csp-sim",
    "csp-phys",
    "csp-view",
    "csp-render",
    "csp-ui",
    "csp-app",
    "csp-script",
    "csp-ai",
    "csp-server",
    "csp-bench",
    "xtask",
];

/// One node of a parsed `cargo tree --format "{p}"` output.
#[derive(Debug)]
pub struct TreeNode {
    /// Exact package name (first token of the `{p}` line; versions,
    /// `(path)`, `(proc-macro)`, `(*)` markers are all stripped).
    pub name: String,
    /// Index into `nodes` of the parent edge — `None` on the depth-0 root.
    pub parent: Option<usize>,
}

/// One crate's parsed `cargo tree` output.
#[derive(Debug)]
pub struct PackageTree {
    /// The crate the tree was generated for (the depth-0 root package).
    pub pkg: String,
    /// `true` when resolved for `wasm32-unknown-unknown` — enables the
    /// wasm-purity rules in [`check_deps`].
    pub wasm: bool,
    pub nodes: Vec<TreeNode>,
}

impl PackageTree {
    /// Package names at depth 1 — the linted crate's own declared deps.
    fn direct_deps(&self) -> Vec<&str> {
        // Depth-1 nodes are the root's children; nodes aren't stored with
        // depth, so reconstruct: a node's depth is 1 iff its parent is the
        // root (index 0).
        self.nodes
            .iter()
            .enumerate()
            .filter(|(i, n)| *i != 0 && n.parent == Some(0))
            .map(|(_, n)| n.name.as_str())
            .collect()
    }

    /// Whether `ancestor_name` is on the parent chain of node `idx`.
    fn has_ancestor(&self, idx: usize, ancestor_name: &str) -> bool {
        let mut cur = self.nodes[idx].parent;
        while let Some(i) = cur {
            if self.nodes[i].name == ancestor_name {
                return true;
            }
            cur = self.nodes[i].parent;
        }
        false
    }
}

/// Parse `cargo tree --format "{p}"` output into a [`PackageTree`].
///
/// Depth is recovered from the tree connectors (`├── `, `└── `, `│   `)
/// so parent edges are exact — this is what lets the wasm `tokio`
/// exception name a *parent* rather than a tree-wide allowlist. `[...]`
/// section headers (`[dev-dependencies]`, `[build-dependencies]`) carry no
/// package and are skipped; under `--edges normal,build` they do not occur.
///
/// Returns `Err` on output that cannot be a tree of `pkg` — including
/// empty output — so a broken `cargo tree` can never read as a pass.
pub fn parse_tree(pkg: &str, wasm: bool, output: &str) -> Result<PackageTree, String> {
    let mut nodes: Vec<TreeNode> = Vec::new();
    // stack[d] = index of the current ancestor node at depth d.
    let mut stack: Vec<usize> = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        // Section headers (`[dev-dependencies]`, `[build-dependencies]`)
        // can appear nested under a `│   ` prefix — look past the
        // tree-drawing column chars, or the header reads as a phantom
        // depth-0 package.
        let behind_columns = trimmed.trim_start_matches(['│', ' ', '─']);
        if behind_columns.is_empty() || behind_columns.starts_with('[') {
            continue;
        }
        // Depth comes from the connector's COLUMN: cargo indents each level
        // by 4 columns ("│   " continuation columns are 4 chars wide — `│`
        // is 3 bytes, so this must be counted in chars, not bytes).
        let (depth, rest) = if let Some(pos) = line.chars().position(|c| c == '├' || c == '└') {
            let rest: String = line.chars().skip(pos + 4).collect();
            (pos / 4 + 1, rest)
        } else {
            (0, trimmed.to_string())
        };
        let name = rest
            .split_whitespace()
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("unparseable cargo tree line: {line:?}"))?
            .to_string();
        if depth == 0 {
            if !nodes.is_empty() {
                return Err(format!("cargo tree -p {pkg}: multiple roots in output"));
            }
        } else if depth > stack.len() {
            // A non-root node with no parent one level up means the tree
            // drawing skipped a depth — malformed input, not an orphan
            // to quietly attach nowhere.
            return Err(format!("cargo tree -p {pkg}: malformed indent at {line:?}"));
        }
        let parent = if depth == 0 {
            None
        } else {
            stack.get(depth - 1).copied()
        };
        let idx = nodes.len();
        nodes.push(TreeNode { name, parent });
        stack.truncate(depth);
        stack.push(idx);
    }
    let root = nodes
        .first()
        .ok_or_else(|| format!("cargo tree -p {pkg}: empty output"))?;
    if root.name != pkg {
        return Err(format!(
            "cargo tree -p {pkg}: output root is {:?}, not the linted crate",
            root.name
        ));
    }
    Ok(PackageTree {
        pkg: pkg.to_string(),
        wasm,
        nodes,
    })
}

/// A single rule violation found by [`check_deps`].
#[derive(Debug)]
pub struct Violation {
    /// The crate whose tree violated.
    pub pkg: String,
    /// Which §2.1/§8.2 rule fired.
    pub rule: &'static str,
    /// The offending package and context.
    pub detail: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} [{}] {}", self.pkg, self.rule, self.detail)
    }
}

/// The pure rule engine. `trees` is one parsed `cargo tree` per checked
/// crate (native and wasm both allowed; `wasm` flags the latter). Returns
/// every violation found — an empty vec is a pass. Kept pure so fixture
/// trees can prove each rule fires.
pub fn check_deps(trees: &[PackageTree]) -> Vec<Violation> {
    let mut out = Vec::new();
    for t in trees {
        // §2.1 deny set for the four determinism crates — every node of the
        // (non-dev) tree, exact names.
        if DETERMINISM_CRATES.contains(&t.pkg.as_str()) {
            for n in &t.nodes {
                for b in DETERMINISM_BANS {
                    if b.name_matches(&n.name) {
                        out.push(Violation {
                            pkg: t.pkg.clone(),
                            rule: "determinism-banned-dep",
                            detail: format!(
                                "depends on {} (banned by §2.1: no bevy_*/wgpu/winit/\
                                 egui/rayon/tokio/getrandom/rand-facade)",
                                n.name
                            ),
                        });
                    }
                }
            }
        }
        // §2.1 positive allowlists — direct deps only.
        if let Some((_, allowed)) = ALLOW.iter().find(|(p, _)| *p == t.pkg) {
            for dep in t.direct_deps() {
                if !allowed.contains(&dep) {
                    out.push(Violation {
                        pkg: t.pkg.clone(),
                        rule: "dep-not-in-allowlist",
                        detail: format!("direct dep {dep} not in the §2.1 allowlist"),
                    });
                }
            }
        }
        // Extra per-crate tree bans (csp-render).
        if let Some((_, bans)) = EXTRA_BANS.iter().find(|(p, _)| *p == t.pkg) {
            for n in &t.nodes {
                for b in *bans {
                    if b.name_matches(&n.name) {
                        out.push(Violation {
                            pkg: t.pkg.clone(),
                            rule: "crate-banned-dep",
                            detail: format!("depends on {} (banned: {})", n.name, b.label()),
                        });
                    }
                }
            }
        }
        // §2.1: "bevy_ecs is allowed only in csp-view". Literal reading —
        // "absent from every other tree" — is impossible because csp-app's
        // tree contains csp-view's; the honest rule is that bevy_ecs may
        // only ENTER a tree through the csp-view edge.
        for (i, n) in t.nodes.iter().enumerate() {
            if n.name == "bevy_ecs" && t.pkg != "csp-view" && !t.has_ancestor(i, "csp-view") {
                out.push(Violation {
                    pkg: t.pkg.clone(),
                    rule: "bevy_ecs-outside-csp-view",
                    detail: "bevy_ecs enters the tree without passing through \
                             csp-view (§2.1: allowed only in csp-view)"
                        .to_string(),
                });
            }
        }
        // §2.1: "csp-view is the only crate allowed to depend on both
        // csp-sim and csp-render" — a direct-dep rule: transitive presence
        // (csp-app gets csp-sim through csp-view) is the normal case.
        if t.pkg != "csp-view" {
            let deps = t.direct_deps();
            if deps.contains(&"csp-sim") && deps.contains(&"csp-render") {
                out.push(Violation {
                    pkg: t.pkg.clone(),
                    rule: "both-sim-and-render",
                    detail: "direct deps on both csp-sim and csp-render — only \
                             csp-view may bridge them (§2.1)"
                        .to_string(),
                });
            }
        }
        // §8.2 wasm purity (positive assertion): the ban list must not
        // appear in the wasm32 tree, with the single documented
        // wasm-bindgen-futures->tokio edge exception.
        if t.wasm {
            for n in &t.nodes {
                for b in WASM_BANS {
                    if !b.name_matches(&n.name) {
                        continue;
                    }
                    if n.name == "tokio" {
                        let parent_ok = n
                            .parent
                            .is_some_and(|p| t.nodes[p].name == TOKIO_ALLOWED_PARENT);
                        if parent_ok {
                            continue;
                        }
                    }
                    let via = n
                        .parent
                        .map(|p| format!(" (via {})", t.nodes[p].name))
                        .unwrap_or_default();
                    out.push(Violation {
                        pkg: t.pkg.clone(),
                        rule: "wasm-impurity",
                        detail: format!("wasm tree contains {}{via} (§8.2 ban)", n.name),
                    });
                }
            }
        }
    }
    out
}

/// Run `cargo tree` for one crate and parse it — `exit(2)` on any tooling
/// or parse failure (§8.2: "fails on tooling error, never passes on empty
/// output").
// xtask is CI tooling, not the determinism domain: a failed cargo tree is
// exactly the loud tooling failure §8.2 wants, so `expect` is the honest
// failure mode (allowed at fn scope).
#[allow(clippy::expect_used)]
fn tree(pkg: &str, wasm: bool) -> PackageTree {
    let mut cmd = Command::new("cargo");
    cmd.args([
        "tree",
        "-p",
        pkg,
        "--edges",
        "normal,build",
        "--format",
        "{p}",
    ]);
    if wasm {
        cmd.args(["--target", "wasm32-unknown-unknown"]);
    }
    let out = cmd.output().expect("cargo tree failed to run");
    if !out.status.success() {
        eprintln!(
            "cargo tree -p {pkg}{} failed:\n{}",
            if wasm { " (wasm32)" } else { "" },
            String::from_utf8_lossy(&out.stderr)
        );
        exit(2);
    }
    let text = String::from_utf8(out.stdout).expect("cargo tree output not utf-8");
    match parse_tree(pkg, wasm, &text) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lint-deps: {e}");
            exit(2);
        }
    }
}

/// `cargo xtask lint-deps`: collect every crate's tree, run the rule
/// engine, print all violations and exit 1 if any exist.
pub fn lint_deps() {
    let mut trees = Vec::new();
    for pkg in ALL_PKGS {
        trees.push(tree(pkg, false));
    }
    for pkg in WASM_PKGS {
        trees.push(tree(pkg, true));
    }
    let violations = check_deps(&trees);
    if violations.is_empty() {
        println!("lint-deps: OK");
    } else {
        for v in &violations {
            eprintln!("VIOLATION: {v}");
        }
        eprintln!("lint-deps: {} violation(s)", violations.len());
        exit(1);
    }
}

// Test code may unwrap — it is the failure path either way.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn t(pkg: &str, wasm: bool, out: &str) -> PackageTree {
        parse_tree(pkg, wasm, out).expect("fixture should parse")
    }

    /// A well-formed fixture violating §2.1 must produce violations — the
    /// negative test proving the gate can fail.
    #[test]
    fn violating_fixtures_produce_violations() {
        // rand facade + a non-allowlisted dep inside csp-sim's tree.
        let bad_sim = t(
            "csp-sim",
            false,
            "csp-sim v0.0.0 (/x)\n\
             ├── csp-core v0.0.0\n\
             └── rand v0.9.2\n",
        );
        let v = check_deps(&[bad_sim]);
        assert!(
            v.iter().any(|x| x.rule == "determinism-banned-dep"),
            "rand facade in csp-sim tree must be a violation: {v:?}"
        );
        assert!(
            v.iter().any(|x| x.rule == "dep-not-in-allowlist"),
            "rand is also outside the §2.1 allowlist: {v:?}"
        );

        // A clean csp-sim tree must produce none.
        let ok_sim = t(
            "csp-sim",
            false,
            "csp-sim v0.0.0 (/x)\n\
             ├── csp-core v0.0.0\n\
             │   ├── blake3 v1.8.3\n\
             │   └── rand_chacha v0.10.0\n\
             └── serde v1.0.229\n",
        );
        assert!(
            check_deps(&[ok_sim]).is_empty(),
            "clean csp-sim tree must pass"
        );
    }

    #[test]
    fn csp_render_must_not_reach_sim() {
        let bad = t(
            "csp-render",
            false,
            "csp-render v0.0.0 (/x)\n\
             ├── wgpu v30.0.0\n\
             └── csp-sim v0.0.0\n",
        );
        assert!(
            check_deps(&[bad])
                .iter()
                .any(|v| v.rule == "crate-banned-dep")
        );
    }

    #[test]
    fn bevy_ecs_only_via_csp_view() {
        // Direct bevy_ecs in csp-ui — violation.
        let bad = t(
            "csp-ui",
            false,
            "csp-ui v0.0.0 (/x)\n├── bevy_ecs v0.20.0\n",
        );
        assert!(
            check_deps(&[bad])
                .iter()
                .any(|v| v.rule == "bevy_ecs-outside-csp-view")
        );
        // bevy_ecs arriving inside csp-app's tree through csp-view — the
        // only legal path. (Depth-2 fixture lines carry `│   ` like real
        // cargo output — a `\` continuation strips plain leading spaces.)
        let ok = t(
            "csp-app",
            false,
            "csp-app v0.0.0 (/x)\n\
             └── csp-view v0.0.0\n\
             │   └── bevy_ecs v0.20.0\n",
        );
        assert!(
            check_deps(&[ok]).is_empty(),
            "bevy_ecs via csp-view is the allowed path"
        );
    }

    #[test]
    fn wasm_purity_and_the_tokio_exception() {
        // tokio under wasm-bindgen-futures — the documented exception.
        let ok = t(
            "csp-app",
            true,
            "csp-app v0.0.0 (/x)\n\
             └── wasm-bindgen-futures v0.4.79\n\
             │   └── tokio v1.54.0\n",
        );
        assert!(
            check_deps(&[ok]).is_empty(),
            "tokio via wasm-bindgen-futures is the only allowed wasm path"
        );
        // tokio via anything else — violation.
        let bad = t(
            "csp-app",
            true,
            "csp-app v0.0.0 (/x)\n\
             └── csp-server v0.0.0\n\
             │   └── tokio v1.54.0\n",
        );
        let v = check_deps(&[bad]);
        assert!(
            v.iter().filter(|x| x.rule == "wasm-impurity").count() == 2,
            "csp-server AND tokio must each fail: {v:?}"
        );
        // The same tree on native is not a wasm-purity matter.
        let native = t(
            "csp-app",
            false,
            "csp-app v0.0.0 (/x)\n\
             └── wasm-bindgen-futures v0.4.79\n\
             │   └── tokio v1.54.0\n",
        );
        assert!(check_deps(&[native]).is_empty());
    }

    #[test]
    fn empty_or_wrong_output_is_an_error_not_a_pass() {
        assert!(parse_tree("csp-sim", false, "").is_err());
        assert!(parse_tree("csp-sim", false, "csp-core v0.0.0\n").is_err());
    }

    /// §8.2 determinism-lints row, second half: the four crates' clippy.toml
    /// files must exist, be byte-identical, and carry every required entry —
    /// a lint that differs between crates is a lint that does not exist.
    #[test]
    fn determinism_clippy_tomls_are_identical_and_complete() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask manifest dir has a parent");
        let crates = ["csp-core", "csp-data", "csp-phys", "csp-sim"];
        let required = [
            // §2.7 RNG ban — seed_from_u64 is a rand_core impl detail.
            "rand_core::SeedableRng::seed_from_u64",
            // The §8.2 libm-enforcement method list.
            "f64::sin",
            "f64::cos",
            "f64::atan2",
            "f64::exp",
            "f64::ln",
            "f64::log",
            "f64::powf",
            "f64::powi",
            "f64::sqrt",
            "f64::cbrt",
            "f64::hypot",
            // §8.2 disallowed types: hash containers + the wall clock.
            "std::collections::HashMap",
            "std::collections::HashSet",
            "std::time::Instant",
            "std::time::SystemTime",
            "std::time::Duration",
        ];
        let mut texts = Vec::new();
        for c in crates {
            let p = root.join(format!("crates/{c}/clippy.toml"));
            let text = std::fs::read_to_string(&p)
                .unwrap_or_else(|e| panic!("{} unreadable: {e}", p.display()));
            for r in required {
                assert!(text.contains(r), "{c}/clippy.toml missing entry {r}");
            }
            texts.push(text);
        }
        assert!(
            texts.windows(2).all(|w| w[0] == w[1]),
            "the four determinism clippy.tomls must be byte-identical"
        );
    }

    /// data/balance.ron (and en.ron) must be real RON — schema_versioned,
    /// named fields, loadable by `ron::from_str`. PLAN §2.3 names
    /// `bubble_radius_m`; PLAN:548 caps advance_to at 2000 events/frame.
    #[test]
    fn balance_ron_is_valid_ron_with_plan_fields() {
        /// The balance fields the plan names (§2.3, PLAN:548). Mirrors what
        /// csp-data's loader will deserialize in M1+.
        #[derive(serde::Deserialize)]
        struct Balance {
            schema_version: u32,
            bubble_radius_m: f64,
            advance_to_max_events_per_frame: u32,
        }

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("xtask manifest dir has a parent");
        let text = std::fs::read_to_string(root.join("data/balance.ron"))
            .expect("data/balance.ron missing");
        let b: Balance = ron::from_str(&text).expect("data/balance.ron is not valid RON");
        // Schema v9 — M8-C added the §3.5 vessel life-support tunables
        // (`life_support_check_period_s` + the three per-crew rates);
        // the loader-side pin is `csp_data::BALANCE_SCHEMA_VERSION`
        // (kept in lockstep by csp-data's own balance gate test).
        assert_eq!(b.schema_version, 9);
        assert_eq!(b.bubble_radius_m, 20_000.0);
        assert_eq!(b.advance_to_max_events_per_frame, 2_000);
        // And the other shipped data file parses too.
        let loc =
            std::fs::read_to_string(root.join("data/loc/en.ron")).expect("data/loc/en.ron missing");
        ron::from_str::<ron::Value>(&loc).expect("data/loc/en.ron is not valid RON");
    }
}
