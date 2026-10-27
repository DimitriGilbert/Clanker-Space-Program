//! The §3.1 **data-set hash** — BLAKE3 over `path_bytes || 0x00 ||
//! u64-LE len || canonicalized_file_bytes`, ascending path order, for
//! every file under `data/` *except*:
//!
//! - everything under `data/test-fixtures/` (§8.3: frozen golden inputs,
//!   deliberately outside the shipping data set — they are hashed
//!   separately by `csp_bench::replay::fixture_hash`, the *fixture* hash
//!   recorded in the §7.6 artifact), and
//! - files under `data/art/` whose extension is in [`ART_EXTENSIONS`]
//!   (textures/audio are presentation assets, never read by the sim).
//!   The same extension **outside** `data/art/` **is** hashed — §3.1 is
//!   explicit that `*.png` counts for biome maps and concentration maps.
//!
//! Boundary, stated because two hashes share the pinned scheme:
//! `data_set_hash` here covers the *shipping* data set and is what
//! `BuiltPart::data_set_hash` / §7.7's drift warning key on;
//! `fixture_hash` in csp-bench covers one frozen scenario dir and lives
//! in the replay artifact. Same construction, disjoint scopes — changing
//! either scheme bumps [`DATA_HASH_VERSION`](crate::DATA_HASH_VERSION).
//!
//! Canonicalization (§3.1): UTF-8 text → LF line endings, trailing
//! whitespace stripped; non-UTF-8 bytes (PNG maps) hash verbatim.
//!
//! §7.7's drift listing: [`data_set_kind_hashes`] runs the same walk
//! feeding one BLAKE3 per [`DataKind`] (the file classes a drift
//! warning can name — "part stats, biome maps, balance constants"),
//! and the save header records the map so a later load's
//! [`changed_kinds`] can enumerate what moved. The kind breakdown is
//! additive metadata — the aggregate [`data_set_hash`] preimage is
//! unchanged, so [`DATA_HASH_VERSION`](crate::DATA_HASH_VERSION)
//! stays put.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

/// Extensions treated as art assets *only when under `data/art/`*
/// (§3.1). Everything else under `data/` — including `*.png` biome and
/// concentration maps — is hashed.
pub const ART_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "tga", "webp", "ktx2", "ogg", "wav", "glb", "csar",
];

/// The §7.7 drift-listing granularity — the coarse file classes a
/// data-set drift warning names ("part stats, biome maps, balance
/// constants"). Every hashed file lands in exactly one kind, so an
/// aggregate-hash drift always names at least one kind.
///
/// The classification is deliberately stable and hand-keyed on the
/// committed `data/` layout (top-level component plus the §3.1
/// image-map rule): kind names are the wire vocabulary of the save
/// header's `data_kind_hashes` map, so a rename is a deliberate
/// compatibility event, not an accident of directory churn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataKind {
    /// `parts/**` — part definitions (§7.7's "part stats").
    PartStats,
    /// `balance.ron` and `resources.ron` — §7.7's "balance
    /// constants": the economy/resource tables a tuning pass touches.
    BalanceConstants,
    /// Files with an [`ART_EXTENSIONS`] extension *outside* `art/` —
    /// the §3.1 biome/concentration maps (§7.7's "biome maps").
    BiomeMaps,
    /// `contracts/**` — contract definitions.
    Contracts,
    /// `experiments/**` — science experiment definitions.
    Experiments,
    /// `recipes/**` — manufacture recipes.
    Recipes,
    /// `techtree/**` — tech tree definitions.
    TechTree,
    /// `systems/**` non-map files — body/site/colony/system defs.
    SystemDefs,
    /// `loc/**` — localization tables.
    Localization,
    /// `eval-scenarios/**` — the eval-tier committed scenarios.
    EvalScenarios,
    /// Everything else the hash covers: `ATTRIBUTION.md`, `LICENSE`,
    /// `id-baseline.ron`, keep-files — meta-content with no sim
    /// meaning, grouped so a docs edit never impersonates a stat
    /// change.
    Metadata,
}

impl DataKind {
    /// Every kind, in declaration (sorted) order — what a full
    /// drift listing iterates.
    pub const ALL: &[Self] = &[
        Self::PartStats,
        Self::BalanceConstants,
        Self::BiomeMaps,
        Self::Contracts,
        Self::Experiments,
        Self::Recipes,
        Self::TechTree,
        Self::SystemDefs,
        Self::Localization,
        Self::EvalScenarios,
        Self::Metadata,
    ];

    /// The save-header map key — kebab-case, stable forever (a save
    /// written by this build must read on later builds).
    pub fn key(self) -> &'static str {
        match self {
            Self::PartStats => "part-stats",
            Self::BalanceConstants => "balance-constants",
            Self::BiomeMaps => "biome-maps",
            Self::Contracts => "contracts",
            Self::Experiments => "experiments",
            Self::Recipes => "recipes",
            Self::TechTree => "tech-tree",
            Self::SystemDefs => "system-defs",
            Self::Localization => "localization",
            Self::EvalScenarios => "eval-scenarios",
            Self::Metadata => "metadata",
        }
    }

    /// Inverse of [`Self::key`]; `None` for names a newer build might
    /// have added — unknown keys are skipped, never a load fault (the
    /// header is the tolerant layer, and the aggregate hash still
    /// catches the drift itself).
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.key() == key)
    }

    /// The human label §7.7's drift notice names ("part stats"…).
    pub fn label(self) -> &'static str {
        match self {
            Self::PartStats => "part stats",
            Self::BalanceConstants => "balance constants",
            Self::BiomeMaps => "biome maps",
            Self::Contracts => "contracts",
            Self::Experiments => "experiments",
            Self::Recipes => "recipes",
            Self::TechTree => "tech tree",
            Self::SystemDefs => "system defs",
            Self::Localization => "localization",
            Self::EvalScenarios => "eval scenarios",
            Self::Metadata => "metadata",
        }
    }
}

/// Classify one hashed file (a `/`-joined path relative to `data/`,
/// already inside the §3.1 scope) into its [`DataKind`]. Keyed on the
/// top-level component — `parts/…` is part stats, `systems/…` is
/// system defs — with the §3.1 image rule applied first: an
/// [`ART_EXTENSIONS`] file outside `art/` is hashed *because* it is a
/// biome/concentration map, so it is its own kind wherever it sits.
fn kind_of(rel: &str) -> DataKind {
    if let Some(ext) = rel.rsplit('.').next()
        && ART_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
    {
        return DataKind::BiomeMaps;
    }
    match rel.split('/').next() {
        Some("parts") => DataKind::PartStats,
        Some("balance.ron" | "resources.ron") => DataKind::BalanceConstants,
        Some("contracts") => DataKind::Contracts,
        Some("experiments") => DataKind::Experiments,
        Some("recipes") => DataKind::Recipes,
        Some("techtree") => DataKind::TechTree,
        Some("systems") => DataKind::SystemDefs,
        Some("loc") => DataKind::Localization,
        Some("eval-scenarios") => DataKind::EvalScenarios,
        _ => DataKind::Metadata,
    }
}

/// `data_set_hash` failures — plain io diagnostics, like the loader's.
#[derive(Debug)]
pub enum DataHashError {
    /// A directory entry or file could not be read.
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A symlink sits inside `data/` — the hash walks the committed
    /// file tree only; a link's target could change outside it.
    Symlink {
        /// The link.
        path: PathBuf,
    },
    /// A file name is not UTF-8 — the hash keys files by path bytes,
    /// and a lossy conversion could collide two distinct names.
    NonUtf8Name {
        /// The file.
        path: PathBuf,
    },
    /// Two paths differ only by ASCII case — the committed tree hashes
    /// both on a case-sensitive filesystem, but a checkout on a
    /// case-insensitive one collapses them into a different set (and a
    /// different hash): the same repo would fingerprint differently by
    /// platform.
    CaseCollision {
        /// The first path (byte-sorted order).
        a: PathBuf,
        /// The colliding path.
        b: PathBuf,
    },
}

impl fmt::Display for DataHashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "io at {}: {source}", path.display()),
            Self::Symlink { path } => {
                write!(f, "symlink in the data set at {}", path.display())
            }
            Self::NonUtf8Name { path } => {
                write!(f, "non-UTF-8 file name at {}", path.display())
            }
            Self::CaseCollision { a, b } => {
                write!(
                    f,
                    "paths differ only by case: {} vs {}",
                    a.display(),
                    b.display()
                )
            }
        }
    }
}

impl std::error::Error for DataHashError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Symlink { .. } | Self::NonUtf8Name { .. } | Self::CaseCollision { .. } => None,
        }
    }
}

/// The two §3.1/§7.7 products of one walk: the aggregate data-set
/// hash and the per-[`DataKind`] breakdown. Callers that need both
/// (a save write stamps both) take this so `data/` is read once.
#[derive(Clone, Debug, PartialEq)]
pub struct DataSetHashes {
    /// The §3.1 aggregate — what [`data_set_hash`] returns.
    pub aggregate: [u8; 32],
    /// The §7.7 per-kind breakdown — what [`data_set_kind_hashes`]
    /// returns.
    pub by_kind: BTreeMap<DataKind, [u8; 32]>,
}

/// Hash the shipping data set rooted at `data_root` (the `data/`
/// directory). Deterministic by construction: paths are sorted before
/// hashing, so filesystem enumeration order is irrelevant.
pub fn data_set_hash(data_root: &Path) -> Result<[u8; 32], DataHashError> {
    Ok(walk_hashes(data_root)?.aggregate)
}

/// Aggregate + per-kind in one filesystem walk — the save-write and
/// bug-report paths' call (one read of `data/`, never two).
pub fn data_set_hashes(data_root: &Path) -> Result<DataSetHashes, DataHashError> {
    walk_hashes(data_root)
}

/// The §7.7 per-kind breakdown of [`data_set_hash`]: one BLAKE3 per
/// [`DataKind`] over the same `path || 0x00 || len || content`
/// framing, restricted to that kind's files. The aggregate hash alone
/// can say *that* the set drifted but never *what* — the save records
/// this map so a later load names the changed kinds ("part stats,
/// biome maps") per §7.7's "warns and lists what changed". `BTreeMap`
/// key order is the deterministic enumeration.
pub fn data_set_kind_hashes(
    data_root: &Path,
) -> Result<BTreeMap<DataKind, [u8; 32]>, DataHashError> {
    Ok(walk_hashes(data_root)?.by_kind)
}

/// §7.7's "lists what changed": the kinds whose hash differs between
/// the save's recorded map and the live data root's. A kind present
/// on only one side counts as changed — its file set appeared or
/// disappeared entirely.
pub fn changed_kinds(
    recorded: &BTreeMap<DataKind, [u8; 32]>,
    current: &BTreeMap<DataKind, [u8; 32]>,
) -> Vec<DataKind> {
    // `ALL` iterates in declaration (Ord) order — the listing is
    // already sorted. A kind this build does not know (recorded by a
    // newer one) has by definition drifted in content terms, but
    // `ALL` cannot name it — the aggregate `!=` check upstream is
    // what still reports the drift at all.
    DataKind::ALL
        .iter()
        .copied()
        .filter(|k| recorded.get(k) != current.get(k))
        .collect()
}

/// One walk over the hashed set feeding both hashers: the §3.1
/// aggregate and the per-kind breakdown. One filesystem traversal,
/// one read per file — the two products never disagree about which
/// bytes they saw.
fn walk_hashes(data_root: &Path) -> Result<DataSetHashes, DataHashError> {
    let mut aggregate = blake3::Hasher::new();
    let mut kinds: BTreeMap<DataKind, blake3::Hasher> = BTreeMap::new();
    for rel in hashed_files(data_root)? {
        let bytes = std::fs::read(data_root.join(&rel)).map_err(|source| DataHashError::Io {
            path: data_root.join(&rel),
            source,
        })?;
        let canonical = canonicalize(&bytes);
        feed_file(&mut aggregate, &rel, &canonical);
        feed_file(kinds.entry(kind_of(&rel)).or_default(), &rel, &canonical);
    }
    Ok(DataSetHashes {
        aggregate: *aggregate.finalize().as_bytes(),
        by_kind: kinds
            .into_iter()
            .map(|(k, h)| (k, *h.finalize().as_bytes()))
            .collect(),
    })
}

/// One file's contribution to the §3.1 stream: `path || 0x00 || len ||
/// content`. The `0x00` terminates the path (filenames cannot carry
/// NUL); the little-endian length prefix then makes the whole frame
/// injective — the earlier `path || 0x00 || content` framing was not
/// (file `a` containing `Xb\0Y` hashed identically to files `a`→`X` and
/// `b`→`Y`), which is why [`crate::DATA_HASH_VERSION`] bumped to 2.
fn feed_file(h: &mut blake3::Hasher, rel: &str, content: &[u8]) {
    h.update(rel.as_bytes());
    h.update(&[0]);
    h.update(&(content.len() as u64).to_le_bytes());
    h.update(content);
}

/// Every hashed file under `data_root` as `/`-joined relative paths in
/// ascending order, after the §3.1 exclusions.
fn hashed_files(root: &Path) -> Result<Vec<String>, DataHashError> {
    let mut out = Vec::new();
    collect(root, "", &mut out)?;
    out.sort();
    // Two paths that differ only by ASCII case hash fine here but
    // collapse to one file on a case-insensitive checkout — the same
    // commit would produce a different data-set hash by platform, so
    // the collision is refused like a symlink or non-UTF-8 name. The
    // fold key is a full-string compare, not an adjacency scan: byte
    // sort does NOT put case-folded-equal paths next to each other
    // (`"FOO.txt" < "Zebra" < "foo.txt"`).
    let mut folded: std::collections::BTreeMap<String, &str> = std::collections::BTreeMap::new();
    for rel in &out {
        let key = rel.to_ascii_lowercase();
        if let Some(first) = folded.insert(key, rel) {
            return Err(DataHashError::CaseCollision {
                a: PathBuf::from(first),
                b: PathBuf::from(rel),
            });
        }
    }
    Ok(out)
}

fn collect(dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), DataHashError> {
    let entries = std::fs::read_dir(dir).map_err(|source| DataHashError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| DataHashError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        // `file_type` does not follow symlinks: a symlink inside
        // `data/` is refused rather than silently traversed — its
        // target could change outside the hashed tree (and a link
        // pointing out of `data/` would escape the set entirely).
        let ft = entry.file_type().map_err(|source| DataHashError::Io {
            path: entry.path(),
            source,
        })?;
        if ft.is_symlink() {
            return Err(DataHashError::Symlink { path: entry.path() });
        }
        // The hash stream keys files by their relative-path BYTES —
        // a name that cannot be represented as UTF-8 has no honest
        // key (lossy conversion could collide two distinct names).
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| DataHashError::NonUtf8Name { path: entry.path() })?;
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        if entry.path().is_dir() {
            // §3.1: data/test-fixtures/ is out of the shipping data
            // set — case-folded like `art/` so a differently-cased
            // checkout excludes the same directory.
            if rel.eq_ignore_ascii_case("test-fixtures") {
                continue;
            }
            collect(&entry.path(), &rel, out)?;
        } else if in_scope(&rel) {
            out.push(rel);
        }
    }
    Ok(())
}

/// §3.1 file inclusion: everything except `test-fixtures/**` (handled in
/// [`collect`]) and `art/**` files with an [`ART_EXTENSIONS`] extension.
/// Both the `art/` prefix and the extension match are
/// ASCII-case-insensitive — `art/x.PNG` is the same presentation asset
/// as `art/x.png` (a case-sensitive compare would hash it, flipping
/// `data_set_hash` on an art-only change), and a `data/Art/` directory
/// *is* `data/art/` on a case-insensitive checkout, so folding keeps
/// the hashed set platform-identical.
fn in_scope(rel: &str) -> bool {
    let in_art = rel.len() > 4 && rel[..4].eq_ignore_ascii_case("art/");
    if in_art
        && let Some(ext) = rel[4..].rsplit('.').next()
        && ART_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e))
    {
        return false;
    }
    true
}

/// §3.1 canonicalization — identical to csp-bench's fixture canonicalizer
/// by design: UTF-8 text → trim-end each line, rejoin with LF; other
/// bytes verbatim.
fn canonicalize(bytes: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(bytes) {
        Ok(text) => text
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .into_bytes(),
        Err(_) => bytes.to_vec(),
    }
}

#[cfg(test)]
// Tests signal failure by panicking; §2.13's no-panic rule covers shipping
// fallible paths, not test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// The framing must be injective: under the pre-v2 scheme
    /// (`path || 0x00 || content`) the file set `{("a", "Xb\0Y")}`
    /// hashed byte-identically to `{("a", "X"), ("b", "Y")}` — one
    /// stream, two file sets. The length prefix separates them.
    #[test]
    fn framing_is_injective_against_content_injected_boundaries() {
        let mut one_file = blake3::Hasher::new();
        feed_file(&mut one_file, "a", b"Xb\0Y");
        let mut two_files = blake3::Hasher::new();
        feed_file(&mut two_files, "a", b"X");
        feed_file(&mut two_files, "b", b"Y");
        assert_ne!(
            one_file.finalize().as_bytes(),
            two_files.finalize().as_bytes(),
            "injected boundary must not collide with the two-file frame"
        );

        // The probe this guards against: both sequences are literally
        // `a\0Xb\0Y` under the retired framing.
        let legacy = |files: &[(&str, &[u8])]| {
            let mut h = blake3::Hasher::new();
            for (p, c) in files {
                h.update(p.as_bytes());
                h.update(&[0]);
                h.update(c);
            }
            *h.finalize().as_bytes()
        };
        assert_eq!(
            legacy(&[("a", b"Xb\0Y")]),
            legacy(&[("a", b"X"), ("b", b"Y")]),
            "the retired framing demonstrably collided"
        );
    }

    /// `art/` exclusion folds ASCII case on both the directory prefix
    /// and the extension — `art/x.PNG` is an art asset, not hashed
    /// data (a case-sensitive match would flip `data_set_hash` on an
    /// art-only change).
    #[test]
    fn in_scope_folds_case_on_art_prefix_and_extension() {
        for rel in [
            "art/tex.png",
            "art/tex.PNG",
            "Art/tex.png",
            "aRt/sub/tex.JpG",
        ] {
            assert!(!in_scope(rel), "{rel} must be excluded as art");
        }
        for rel in [
            "art/tex.ron",      // not an art extension — data
            "artistic/tex.png", // not the art dir
            "art",              // not under art/
            "artx/tex.png",
            "biomes/terrin.png", // png outside art/ IS hashed (§3.1)
        ] {
            assert!(in_scope(rel), "{rel} must be hashed");
        }
    }

    /// Two files whose paths differ only by case collide on a
    /// case-insensitive checkout — the same commit would fingerprint
    /// differently by platform, so the hash refuses them.
    #[test]
    fn case_colliding_paths_are_refused() {
        let root = std::env::temp_dir().join(format!("csp-datahash-case-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("parts")).unwrap();
        std::fs::write(root.join("parts/FOO.ron"), b"a").unwrap();
        std::fs::write(root.join("parts/foo.ron"), b"b").unwrap();
        let err = data_set_hash(&root).unwrap_err();
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            matches!(err, DataHashError::CaseCollision { .. }),
            "got {err:?}"
        );
    }

    /// A scratch `data/` tree under the temp dir — returns its root
    /// for cleanup. Each call uses a unique name so parallel tests
    /// never share a tree.
    fn scratch_root(tag: &str, files: &[(&str, &[u8])]) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "csp-datahash-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, bytes) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
        }
        root
    }

    /// §7.7's granularity: every hashed file lands in exactly the
    /// kind its directory (or the §3.1 image rule) names.
    #[test]
    fn kind_of_classifies_the_committed_layout() {
        for (rel, want) in [
            ("parts/csp.pod.mk1.ron", DataKind::PartStats),
            ("balance.ron", DataKind::BalanceConstants),
            ("resources.ron", DataKind::BalanceConstants),
            // §3.1: image extensions outside `art/` are the hashed
            // maps — their own kind wherever they sit.
            ("systems/home/terrin/biomes.png", DataKind::BiomeMaps),
            ("systems/home/terrin/biomes.PNG", DataKind::BiomeMaps),
            ("systems/home/system.ron", DataKind::SystemDefs),
            ("contracts/contract.survey.ron", DataKind::Contracts),
            ("experiments/exp.x.ron", DataKind::Experiments),
            ("recipes/recipe.smelt_ore.ron", DataKind::Recipes),
            ("techtree/home.ron", DataKind::TechTree),
            ("loc/en.ron", DataKind::Localization),
            ("eval-scenarios/ascent-mission.ron", DataKind::EvalScenarios),
            ("ATTRIBUTION.md", DataKind::Metadata),
            ("id-baseline.ron", DataKind::Metadata),
            // A non-art-extension file under `art/` IS hashed (the
            // §3.1 rule cuts on extension, not directory alone) and
            // has no better kind than Metadata.
            ("art/pack.ron", DataKind::Metadata),
        ] {
            assert_eq!(kind_of(rel), want, "{rel}");
        }
    }

    /// `key`/`from_key` round-trip and uniqueness — the save header's
    /// wire vocabulary must be one-to-one.
    #[test]
    fn data_kind_keys_round_trip_and_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for k in DataKind::ALL {
            assert!(seen.insert(k.key()), "duplicate key {}", k.key());
            assert_eq!(DataKind::from_key(k.key()), Some(*k));
        }
        assert_eq!(DataKind::from_key("bogus-kind"), None);
    }

    /// The per-kind breakdown buckets by `kind_of`, feeds the same
    /// framing, and leaves `data_set_hash` untouched — the aggregate
    /// over a tree is identical whether or not kinds are computed.
    #[test]
    fn kind_hashes_partition_the_aggregate_walk() {
        let root = scratch_root(
            "kinds",
            &[
                ("parts/a.ron", b"part a"),
                ("parts/b.ron", b"part b"),
                ("systems/home/x/biomes.png", &[1u8; 32]),
                ("balance.ron", b"balance"),
                ("README.txt", b"meta"),
            ],
        );
        let kinds = data_set_kind_hashes(&root).unwrap();
        // No `systems/*.ron` was written — the png is the only
        // systems file, so `SystemDefs` is absent, not empty.
        assert_eq!(
            kinds.keys().copied().collect::<Vec<_>>(),
            vec![
                DataKind::PartStats,
                DataKind::BalanceConstants,
                DataKind::BiomeMaps,
                DataKind::Metadata,
            ],
        );
        // One kind's files changed → only that kind's hash moves; the
        // aggregate still moves (any change is a data-set change).
        let before_agg = data_set_hash(&root).unwrap();
        let before_kinds = kinds.clone();
        std::fs::write(root.join("parts/a.ron"), b"part a CHANGED").unwrap();
        let after_agg = data_set_hash(&root).unwrap();
        let after_kinds = data_set_kind_hashes(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert_ne!(before_agg, after_agg);
        for k in DataKind::ALL {
            if *k == DataKind::PartStats {
                assert_ne!(before_kinds[k], after_kinds[k]);
            } else {
                assert_eq!(
                    before_kinds.get(k),
                    after_kinds.get(k),
                    "{k:?} must not move on a parts edit"
                );
            }
        }
        // `changed_kinds` names exactly the touched kind.
        assert_eq!(
            changed_kinds(&before_kinds, &after_kinds),
            vec![DataKind::PartStats]
        );
    }

    /// A kind that appears or disappears between the recorded and
    /// current maps counts as changed (its file set did).
    #[test]
    fn changed_kinds_flags_added_and_removed_kinds() {
        let recorded: BTreeMap<DataKind, [u8; 32]> =
            [(DataKind::PartStats, [1; 32])].into_iter().collect();
        let current: BTreeMap<DataKind, [u8; 32]> = [
            (DataKind::PartStats, [1; 32]),
            (DataKind::Contracts, [2; 32]),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            changed_kinds(&recorded, &current),
            vec![DataKind::Contracts]
        );
        assert_eq!(
            changed_kinds(&current, &recorded),
            vec![DataKind::Contracts]
        );
        assert!(changed_kinds(&current, &current).is_empty());
    }

    /// An uppercase art extension is excluded end-to-end: hashing a
    /// tree containing only `art/x.PNG` yields the empty-set hash.
    #[test]
    fn uppercase_art_extension_hashes_nothing() {
        let root = std::env::temp_dir().join(format!("csp-datahash-png-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("art")).unwrap();
        std::fs::write(root.join("art/x.PNG"), [0u8; 16]).unwrap();
        let h = data_set_hash(&root).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        let empty = {
            let r = std::env::temp_dir().join(format!("csp-datahash-empty-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&r);
            std::fs::create_dir_all(&r).unwrap();
            let h = data_set_hash(&r).unwrap();
            let _ = std::fs::remove_dir_all(&r);
            h
        };
        assert_eq!(h, empty, "an art-only tree must hash as the empty set");
    }
}
