//! `data/loc/en.ron` — the v1 English-only localization table (§1.2).
//! `LocKey`s in data files are indirection into this map; the loader and
//! validator can therefore check that every referenced key resolves.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One locale file: `schema_version` first (§3.1), then a key → text map.
/// `BTreeMap` keeps iteration order deterministic (§2.2 bans hash maps in
/// this crate).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocFile {
    /// First field of every data file (§3.1). Must equal
    /// [`crate::system::SCHEMA_VERSION`].
    pub schema_version: u32,
    /// `LocKey` text → English string. Keys use dotted paths, e.g.
    /// `body.terrin.name`, `biome.terrin.ocean`.
    pub strings: BTreeMap<String, String>,
}

impl LocFile {
    /// Whether `key` resolves in this locale file.
    pub fn contains(&self, key: &crate::ids::LocKey) -> bool {
        self.strings.contains_key(key.as_str())
    }
}
