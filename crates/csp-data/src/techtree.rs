//! The §3.3 tech-tree schema — `data/techtree/*.ron`.
//!
//! # The model
//!
//! A [`TechNode`] costs science and, per §3.3's sentence verbatim,
//! "unlocks new `PartId`s, `UpgradeTrack` levels, facilities, and
//! contract types". Those four unlocks land on four different
//! mechanisms, deliberately — the schema documents which channel each
//! rides so no field is a second source of truth:
//!
//! - **`PartId`s — via `PartDef::tech_requirement`, not a node field.**
//!   The part file itself names the node that unlocks it (§3.1); a
//!   `parts:` list on the node could only ever disagree with it. The
//!   "TechNode unlocks PartIds" reading is realized by the
//!   `tech_requirement` edge plus the validator's §3.3 rule "every
//!   part reachable" (every part's `tech_requirement` names a node in
//!   the tree). The editor's "what does this node unlock" panel scans
//!   the same edge the other direction — one edge, read both ways.
//! - **`UpgradeTrack` levels — via `TechUnlocks::track_levels`, a
//!   level *cap* grant.** Each [`UpgradeLevel`] authors its own
//!   `science_cost`, so the node's unlock is the *right to purchase*,
//!   not the purchase: a node granting `(track, 2)` makes levels 1–2
//!   buyable, and the sim's `ResearchUpgradeLevel` command spends
//!   `levels[current].science_cost` per level. That is the only
//!   reading under which the per-level `science_cost` does real work —
//!   the documented interpretation this schema encodes.
//! - **Facilities / contract types — opaque unlock markers.** The
//!   facility defs land with the facility economy milestone and the
//!   contract templates with §3.6; until then the validator checks
//!   well-formedness only.
//!
//! # Tracks and the frozen-at-build contract (§3.3/§3.1)
//!
//! [`UpgradeTrack`] is `{ family, stat_path, levels }` verbatim —
//! "multiplicative deltas in data, never new `PartDef`s". Levels are
//! composed into a frozen `TrackLevels { levels, multiplier }` map on
//! the `BuiltPart` **at manufacture** (`csp_phys::resolve`'s
//! `compose_upgrade_levels`): `Multiplier` levels fold by product, and
//! `Delta` levels fold as `(base + Σ deltas) / base` against the
//! *authored* base stat ([`StatPath::base_value`]) — the delta rides
//! scale and variant afterwards like any multiplier, so an additive
//! upgrade scales with the part it is frozen onto. Either way the
//! `BuiltPart` record keeps the composed effect verbatim, so a
//! rebalanced or deleted track can never alter an in-flight part —
//! §3.3's "never retroactively to craft already in flight, including
//! across a reload".
//!
//! # Files and merging
//!
//! Each `data/techtree/*.ron` file is a [`TechTreeDef`] *fragment* —
//! one tree partitioned across files for authorship (a `rocketry.ron`
//! next to a `science.ron`). [`crate::load::load_techtree`] walks the
//! directory in ascending-path order and [`TechTree::merged`] flattens
//! the fragments into the lookup view the sim installs; the validator
//! ([`crate::validate::validate_techtree`]) enforces the set rules —
//! no cycles, unique ids, prerequisites resolvable, costs monotonic in
//! depth, every part reachable, every track grant/stat/family sane —
//! across the merged set.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ids::{ContractTypeId, FacilityId, FamilyId, LocKey, TechNodeId, UpgradeTrackId};
use crate::part::{PartDef, StatPath};

/// The `schema_version` this loader understands for
/// `data/techtree/*.ron` — its own namespace (§3.1 first-field
/// convention).
pub const TECHTREE_SCHEMA_VERSION: u32 = 1;

/// One `data/techtree/*.ron` file — a fragment of the program's single
/// tree (see the module doc). Identity rules live on the merged set:
/// the fragment carries no id of its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TechTreeDef {
    /// First field of every data file (§3.1). Must equal
    /// [`TECHTREE_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The fragment's nodes.
    pub nodes: Vec<TechNode>,
    /// The fragment's upgrade tracks.
    pub tracks: Vec<UpgradeTrack>,
}

/// §3.3 `TechNode` — one purchasable node of the tree. `requires` is
/// the prerequisite edge set: every entry must itself be researched
/// before this node may be bought (the validator enforces acyclicity,
/// so an empty `requires` is a root and every chain terminates at
/// one).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TechNode {
    /// Stable node id (`tech.rocketry.2`). Part defs name it through
    /// `PartDef::tech_requirement`.
    pub id: TechNodeId,
    /// Loc key for the display name.
    pub display_name: LocKey,
    /// Loc key for the description blurb.
    pub description: LocKey,
    /// Science points the `ResearchTech` command spends to unlock the
    /// node (≥ 0; `0` is the legal starter-node case).
    pub science_cost: f64,
    /// Prerequisite node ids — every entry must be researched first.
    /// Validator: refs exist, no duplicates, no cycles, and each
    /// entry's `science_cost` does not exceed this node's ("cost
    /// monotonic in depth", §3.3).
    pub requires: Vec<TechNodeId>,
    /// What researching the node unlocks (see [`TechUnlocks`] — part
    /// unlocks ride `PartDef::tech_requirement`, deliberately not
    /// redeclared here).
    pub unlocks: TechUnlocks,
}

/// A node's unlock set minus parts (module doc: parts gate on
/// `PartDef::tech_requirement`, which the validator checks against the
/// tree as the "every part reachable" rule).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TechUnlocks {
    /// `(track, cap)` grants — researching the node raises the
    /// purchasable level cap on `track` to `cap` (max over every
    /// researched node's grants). `cap` is a 1-based level count: `2`
    /// makes `levels[0..2]` buyable. Levels themselves still cost
    /// their authored `science_cost` each — see the module doc.
    pub track_levels: Vec<(UpgradeTrackId, u8)>,
    /// Facilities the node unlocks (§3.3) — opaque until the facility
    /// economy milestone; the validator checks well-formedness only.
    pub facilities: Vec<FacilityId>,
    /// Contract types the node unlocks (§3.3/§3.6) — opaque until the
    /// contracts milestone; well-formedness only.
    pub contract_types: Vec<ContractTypeId>,
}

/// One purchasable rung of an [`UpgradeTrack`] — §3.3's
/// `{ science_cost, multiplier_or_delta }` verbatim.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeLevel {
    /// Science points the `ResearchUpgradeLevel` command spends to buy
    /// exactly this rung (validator: finite, ≥ 0).
    pub science_cost: f64,
    /// §3.3's `multiplier_or_delta` — the effect the level contributes
    /// when composed at manufacture (see [`UpgradeEffect`]).
    pub multiplier_or_delta: UpgradeEffect,
}

/// The two effect spellings §3.3's `multiplier_or_delta` names.
/// Composition order is fixed: deltas to the authored base first
/// (`(base + ΣΔ)/base`), then the product of multipliers — see the
/// module doc's composition note.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum UpgradeEffect {
    /// Multiplicative factor — folds by product
    /// (`stat · Π multipliers`). Must be finite and `> 0` (validator).
    Multiplier(f64),
    /// Additive delta on the authored base stat — folds as
    /// `(base + Σ deltas) / base` at manufacture, then applies like a
    /// multiplier. Must be finite (validator); a composed total at or
    /// below zero is a manufacture error.
    Delta(f64),
}

/// §3.3 `UpgradeTrack` — `{ family, stat_path, levels }` verbatim:
/// the purchasable sequence of stat improvements every part of
/// `family` picks up at manufacture. Buying is sequential — level `n`
/// needs levels `1..n` bought and a node-granted cap of at least `n`.
///
/// `id` is a stable string like every data id; the validator also
/// enforces `(family, stat_path)` uniqueness across tracks so the pair
/// identifies a track unambiguously (two tracks folding onto the same
/// stat of the same family would be redundant authored surface).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeTrack {
    /// Stable track id (`csp.track.engine.launch.isp`) — node grants
    /// and the `ResearchUpgradeLevel` command name it.
    pub id: UpgradeTrackId,
    /// Loc key for the display name.
    pub display_name: LocKey,
    /// The part family the track improves (`PartDef::family`).
    pub family: FamilyId,
    /// The scalar stat the levels act on — must resolve on *every*
    /// part of the family (validator; the resolver errors on an
    /// unresolvable frozen upgrade otherwise).
    pub stat_path: StatPath,
    /// The purchasable rungs, in buy order. Validator: non-empty,
    /// `science_cost` finite and ≥ 0, effects sane
    /// ([`UpgradeEffect`]'s bounds), and `len ≤ u8::MAX` (the grant
    /// and `TechState` counters are `u8`).
    pub levels: Vec<UpgradeLevel>,
}

/// The merged lookup view over every loaded `data/techtree/*.ron`
/// fragment — what `World::set_tech_tree` installs as construction
/// config. Iteration order is the loader's ascending-path file order
/// (deterministic); lookup helpers are plain scans — the v1 tree is
/// tens of nodes, not thousands.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TechTree {
    /// Every fragment's nodes, concatenated in file order.
    pub nodes: Vec<TechNode>,
    /// Every fragment's tracks, concatenated in file order.
    pub tracks: Vec<UpgradeTrack>,
}

impl TechTree {
    /// The node named `id`, first match in merged order (the
    /// validator forbids duplicates; an unvalidated tree could carry
    /// them — first wins, deterministically).
    pub fn node(&self, id: &TechNodeId) -> Option<&TechNode> {
        self.nodes.iter().find(|n| &n.id == id)
    }

    /// The track named `id` — same first-match convention.
    pub fn track(&self, id: &UpgradeTrackId) -> Option<&UpgradeTrack> {
        self.tracks.iter().find(|t| &t.id == id)
    }

    /// The tracks that apply to `family`, in merged order — the set
    /// `csp_phys::resolve::compose_upgrade_levels` walks at
    /// manufacture.
    pub fn tracks_for_family<'a>(
        &'a self,
        family: &'a FamilyId,
    ) -> impl Iterator<Item = &'a UpgradeTrack> {
        self.tracks.iter().filter(move |t| &t.family == family)
    }

    /// The level cap `researched` grants on `track` — the maximum
    /// over every researched node's `track_levels` grants (0 = the
    /// track is not yet purchasable).
    pub fn level_cap(&self, track: &UpgradeTrackId, researched: &BTreeSet<TechNodeId>) -> u8 {
        self.nodes
            .iter()
            .filter(|n| researched.contains(&n.id))
            .flat_map(|n| n.unlocks.track_levels.iter())
            .filter(|(t, _)| t == track)
            .map(|(_, cap)| *cap)
            .max()
            .unwrap_or(0)
    }

    /// Whether `researched` unlocks `def` — the `tech_requirement`
    /// edge read in the availability direction.
    pub fn part_unlocked(&self, def: &PartDef, researched: &BTreeSet<TechNodeId>) -> bool {
        researched.contains(&def.tech_requirement)
    }
}
