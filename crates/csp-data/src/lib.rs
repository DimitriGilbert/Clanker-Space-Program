#![forbid(unsafe_code)]

//! Data definitions, loaders, frozen save schemas + migrations (PLAN §2.1, §7).

/// Version of the data-set hashing scheme (PLAN §3.1). Recorded in the §7.1
/// determinism fingerprint (and later the §7.2 save header) so a scheme
/// change explains — rather than silently red-dening — every golden replay.
///
/// The pinned scheme: BLAKE3-256 over the concatenation, in ascending path
/// order, of `path_bytes || 0x00 || canonicalized_file_bytes` for every file
/// in the hashed set, where canonicalization is LF line endings with
/// trailing whitespace stripped (§3.1). `csp-bench` applies the same
/// construction to a frozen `data/test-fixtures/<scenario>/` directory to
/// produce the replay artifact's `data_fixture_hash` (§7.6).
///
/// Bump this in the same commit that changes the scheme, and say why.
pub const DATA_HASH_VERSION: u32 = 1;
