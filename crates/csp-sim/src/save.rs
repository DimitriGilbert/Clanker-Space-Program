//! The §7.7 versioned save format on the §7.2-adjudicated wire container.
//!
//! Wire layout — a plaintext, uncompressed, self-describing JSON header
//! followed by a self-describing body (§7.2's adjudicated format):
//!
//! ```text
//! CSPSAVE\0 | header_len: u32 LE | UTF-8 JSON header | DEFLATE(CBOR body)
//! ```
//!
//! The header is deliberately plaintext + self-describing: a hex editor
//! or a bug-report bundle tells a reader which frozen schema the body
//! uses, which fingerprint versions it was written under, and which
//! codecs the body needs — all without decoding it:
//!
//! ```json
//! { "save_version": 2, "game_version": "…", "hash_schema_version": N,
//!   "kdf_version": N, "data_hash_version": N, "data_set_hash": "<hex>|null",
//!   "label": "…|null", "compression": "deflate", "body_format": "cbor",
//!   "body_len": N }
//! ```
//!
//! `body_len` is the *compressed* body length — a framing field so a
//! truncated or appended file is refused before the codec runs.
//!
//! The body is DEFLATE (`miniz_oxide`, zlib framing — the same codec the
//! csp-data PNG decoder inflates with) over **canonical CBOR**
//! (`ciborium`): struct fields serialize in declaration order, `BTreeMap`
//! in sorted-key order, so identical saves produce identical bytes. §7.2
//! rejected bincode for saves deliberately: a self-describing body makes
//! field-tolerant decode possible, and it costs no determinism because
//! the §7.1 state hash comes from the `StateHasher` visitor, never from
//! the serializer. (The §7.6 command log stays bincode — it is
//! version-scoped by T3 and append-heavy.)
//!
//! `save_version` selects the *frozen* schema the CBOR body decodes as:
//!
//! | version | body |
//! |---|---|
//! | 1 | the first public build's schema — **not loadable**: the v1 |
//! |   |   snapshot was declared with live shared types, so it drifted |
//! |   |   with the tree instead of freezing (genuine v1 bodies predate |
//! |   |   `WeldPart::engine` and several later fields and match no |
//!   |   | schema this build can name). [`SAVE_VERSION_MIN`] is 2; a |
//! |   |   stamped-1 file is refused `UnsupportedVersion` — the honest |
//! |   |   answer, since no frozen v1 types exist to migrate. |
//! | 2 | the live [`WorldSnapshot`] body stamped `2` — accepted only |
//! |   |   when it byte-exact-decodes under the current schema (see |
//! |   |   below) |
//! | 3 | [`SaveBodyV3`] — the live [`WorldSnapshot`] |
//! |   |   (v2's envelope additions — label + data-set hash — live in |
//! |   |   the JSON header, not the body) |
//!
//! v2 and v3 share one body schema: the stamp failed to advance when
//! `WeldPart::engine` grew the serialized `Vessel` tree, so a
//! stamped-2 body is ambiguous — it may be a post-engine write (which
//! decodes cleanly and loads) or a pre-engine write (which cannot be
//! resolved — serde would field-default `engine` to `None`, silently
//! unmetering every resolved engine). The dispatch therefore proves
//! the stamped-2 body against the schema it claims: a body that fails
//! the byte-exact re-encode gate is refused
//! [`SaveError::UnresolvableSchema`] — the honest "the stamp is in
//! range but the schema cannot be resolved" surface, never the
//! cryptic `TrailingBytes` the gate alone produced (and never the
//! silent `None` decode that made pre-engine saves restore with
//! free-burning engines). A `save_version` outside the chain is
//! refused [`SaveError::UnsupportedVersion`].
//!
//! **Adding a version:** freeze the previous snapshot into a `vN` module
//! (a *copy* — frozen types are never edited, §7.7), write
//! `migrate_vN_to_vN+1` returning the next version's type, bump
//! [`SAVE_VERSION_CURRENT`], and extend [`decode_body`]'s dispatch. A
//! save whose version cannot be resolved is refused with
//! [`SaveError::UnsupportedVersion`] — never silently half-loaded.
//!
//! This module is the migration harness only; the *file* lives wherever
//! the caller puts it (`csp-app` uses `$config_dir/csp/`, §7.8's sidecar
//! class — the save itself is authoritative, unlike the other sidecars).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{CommandKey, SimCommand, WorldSnapshot};

/// `save_version` written by this build (§7.7). The chain begins at
/// version 2 — see the module doc's version table for why version 1
/// is refused rather than migrated (no frozen v1 types were ever
/// written: the v1 struct aliased the live tree, so nothing here can
/// decode a genuine v1 body).
/// Version 3 carries the same body schema as 2 — the bump marks the
/// `WeldPart::engine` payload change the `2` stamp missed.
pub const SAVE_VERSION_CURRENT: u32 = 3;
/// The oldest `save_version` this build can read.
pub const SAVE_VERSION_MIN: u32 = 2;
/// The §7.2 magic — the first 8 bytes of every save file, `CSPSAVE`
/// plus its NUL terminator so the magic is never a prefix of a name.
pub const SAVE_MAGIC: &[u8; 8] = b"CSPSAVE\0";

/// The `compression` header value this build reads and writes.
const COMPRESSION_DEFLATE: &str = "deflate";
/// The `body_format` header value this build reads and writes.
const BODY_FORMAT_CBOR: &str = "cbor";
/// DEFLATE level — pinned in code (not negotiated) so identical saves
/// compress to identical bytes.
const DEFLATE_LEVEL: u8 = 6;
/// Inflate bound: the compressed body can be tiny while the plaintext
/// is not — a corrupt or hostile save (a DEFLATE bomb) is refused at
/// 256 MiB rather than OOMing the loader. The M9c snapshot-size budget
/// is 32 MiB, so the bound is an order of magnitude above the real
/// envelope.
const MAX_INFLATED_BODY: usize = 256 << 20;

/// The bincode wire configuration for the §7.6 command-log artifact —
/// replay logs stay bincode (§7.2's other ruling: they are
/// version-scoped by T3 and append-heavy). `standard` is bincode 2.x's
/// canonical default; it reports bytes read rather than rejecting
/// trailing bytes, so [`decode_command_log`] performs that check itself.
fn bincode_config() -> bincode::config::Configuration {
    bincode::config::standard()
}

/// A decoded save file at the *current* schema — what [`decode_save`]
/// returns and [`encode_save`] consumes. The envelope fields
/// (`game_version`, `label`, `data_set_hash`, `state_hash`, `tick`)
/// live in the JSON header (§7.2) but are surfaced here so callers
/// never re-parse.
#[derive(Clone, Debug, PartialEq)]
pub struct Save {
    /// The writer's `game_version` string from the JSON header.
    pub game_version: String,
    /// The save slot's display label (v2+; `None` migrated from v1).
    /// User metadata — never hashed, never fed back into sim state.
    pub label: Option<String>,
    /// The §3.1 data-set hash the save was written against (v2+).
    /// §7.7's drift-warning key: `None` migrated from v1 means "unknown
    /// data set — the loader cannot check drift and says so".
    pub data_set_hash: Option<[u8; 32]>,
    /// The world's §7.1 state hash at save time (v3-header writers).
    /// Loader-side integrity metadata only — never fed back into sim
    /// state: a restored world whose `state_hash()` differs from this
    /// is a §7.5 divergence made loud, and slot listings can show it
    /// without decoding the body.
    pub state_hash: Option<[u8; 32]>,
    /// The §7.2 serialized set at the current schema.
    pub snapshot: WorldSnapshot,
}

/// The JSON header (§7.2) — written verbatim after the magic + length
/// prefix. Carries every fingerprint version that can change bytes
/// without a sim bug, so a report can diagnose "same save, different
/// toolchain" without decoding the body.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct SaveHeader {
    /// The frozen schema the body decodes as — the chain key.
    save_version: u32,
    /// The build that wrote the file (§7.7's second plaintext field).
    game_version: String,
    /// `csp_core::HASH_SCHEMA_VERSION` at write time.
    hash_schema_version: u32,
    /// `csp_core::KDF_VERSION` at write time.
    kdf_version: u8,
    /// `csp_data::DATA_HASH_VERSION` at write time.
    data_hash_version: u32,
    /// The §3.1 data-set hash, lowercase hex; `None`/absent on v1 files
    /// (a v1 writer did not know the hash — "unknown" is honest).
    #[serde(default)]
    data_set_hash: Option<String>,
    /// The save slot's display label — user metadata, never hashed.
    /// Absent/`null` on v1 files (the field did not exist yet).
    #[serde(default)]
    label: Option<String>,
    /// The snapshot's `clock_ticks` — envelope metadata so a slot
    /// listing can show the save's tick without inflating the body.
    /// Absent on headers written before the field existed.
    #[serde(default)]
    tick: Option<u64>,
    /// The world's §7.1 state hash at save time, lowercase hex —
    /// loader-side integrity metadata (see [`Save::state_hash`]).
    /// Absent on headers written before the field existed.
    #[serde(default)]
    state_hash: Option<String>,
    /// The body codec — `"deflate"` today; anything else is
    /// [`SaveError::UnsupportedEncoding`], not a decode guess.
    compression: String,
    /// The decoded body's serialization — `"cbor"` today.
    body_format: String,
    /// The compressed body length in bytes — the framing check.
    body_len: u64,
}

/// The v3 body — the live snapshot under the v3 schema's name. Frozen
/// the moment v4 lands (it becomes `v3`'s copy and the live body moves
/// on). The envelope metadata v2 added over v1 (label, data-set hash)
/// lives in the JSON header per §7.2, so the body is the snapshot alone.
/// Stamped-2 bodies decode under this same schema — see the module
/// doc's version table for the pre-/post-`engine` ambiguity rule.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SaveBodyV3 {
    /// The §7.2 set at the live schema.
    snapshot: WorldSnapshot,
}

/// Failures of the save codec — refused loudly per §7.7 ("a save that
/// cannot be migrated is refused with a message, never half-loaded").
#[derive(Debug)]
pub enum SaveError {
    /// The file does not begin with the `CSPSAVE\0` magic.
    BadMagic,
    /// The header is malformed — truncated `header_len`, non-UTF-8 or
    /// non-JSON bytes, a missing required field, or a `data_set_hash`
    /// that is not 64 lowercase-hex characters.
    BadHeader,
    /// The JSON header could not be serialized — not reachable for the
    /// fields this build writes, kept for completeness.
    HeaderEncode(serde_json::Error),
    /// The JSON header did not parse.
    HeaderDecode(serde_json::Error),
    /// `save_version` outside [`SAVE_VERSION_MIN`..=`SAVE_VERSION_CURRENT`]
    /// — an older-than-the-chain or newer-than-this-build save. The
    /// message names both sides, per §7.7's refusal contract.
    UnsupportedVersion {
        /// The `save_version` the file declared.
        found: u32,
    },
    /// The stamp is inside the chain but the body failed the
    /// byte-exact decode proof under the schema its stamp claims —
    /// the stamp predates a serialized-field change its version bump
    /// missed (the stamped-2 ambiguity: pre-`WeldPart::engine`
    /// bodies). The schema is unresolvable, not the version out of
    /// range — the message says which.
    UnresolvableSchema {
        /// The `save_version` the file declared.
        found: u32,
    },
    /// The header names a `compression`/`body_format` this build does
    /// not speak — a future save's codec change is refused by name,
    /// not guessed at.
    UnsupportedEncoding {
        /// The `compression` field the file declared.
        compression: String,
        /// The `body_format` field the file declared.
        body_format: String,
    },
    /// CBOR encode of the versioned body failed.
    BodyEncode(ciborium::ser::Error<std::io::Error>),
    /// CBOR decode of the versioned body failed.
    BodyDecode(ciborium::de::Error<std::io::Error>),
    /// DEFLATE inflate of the body failed — corrupt stream or the
    /// [`MAX_INFLATED_BODY`] bound was hit.
    Inflate(miniz_oxide::inflate::DecompressError),
    /// `body_len` disagreed with the file, or the inflated bytes were
    /// not exactly one canonical-CBOR value — a corrupt or foreign file.
    TrailingBytes,
    /// The §7.6 command-log artifact (bincode, not the save codec)
    /// failed to encode.
    LogEncode(bincode::error::EncodeError),
    /// The §7.6 command-log artifact failed to decode.
    LogDecode(bincode::error::DecodeError),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "not a CSPSAVE file (missing CSPSAVE\\0 magic)"),
            Self::BadHeader => write!(f, "malformed CSPSAVE header"),
            Self::HeaderEncode(e) => write!(f, "save header encode: {e}"),
            Self::HeaderDecode(e) => write!(f, "save header decode: {e}"),
            Self::UnsupportedVersion { found } => write!(
                f,
                "save version {found} is outside this build's chain \
                 ({SAVE_VERSION_MIN}..={SAVE_VERSION_CURRENT})"
            ),
            Self::UnresolvableSchema { found } => write!(
                f,
                "save version {found} body does not decode under this \
                 build's v{found} schema — it predates serialized \
                 fields the {found} stamp missed, and no frozen \
                 v{found} types exist to migrate it"
            ),
            Self::UnsupportedEncoding {
                compression,
                body_format,
            } => write!(
                f,
                "unsupported save encoding (compression: {compression:?}, \
                 body_format: {body_format:?})"
            ),
            Self::BodyEncode(e) => write!(f, "save body encode: {e}"),
            Self::BodyDecode(e) => write!(f, "save body decode: {e}"),
            Self::Inflate(e) => write!(f, "save body inflate: {e}"),
            Self::TrailingBytes => write!(
                f,
                "body framing mismatch — trailing, truncated, or \
                 non-canonical bytes"
            ),
            Self::LogEncode(e) => write!(f, "command-log encode: {e}"),
            Self::LogDecode(e) => write!(f, "command-log decode: {e}"),
        }
    }
}

impl std::error::Error for SaveError {}

/// The body decoded under a specific schema version — the dispatch
/// product [`decode_body`] feeds into the chain fold. Each entry names
/// its frozen type, which is what keeps the fold honest: versions are
/// structs, not branches over live types.
enum VersionedBody {
    /// `save_version` 3 (and proven stamped-2 bodies — the schemas are
    /// identical) — the current body wrapping the live snapshot.
    V3(SaveBodyV3),
}

/// Encode a hash as lowercase hex — the JSON header's `data_set_hash`
/// representation (hex keeps the header human-diffable; the binary hash
/// would be 64 unreadable JSON array entries).
fn hex_encode(hash: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(64);
    for b in hash {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0F) as usize] as char);
    }
    s
}

/// Inverse of [`hex_encode`]; `None` on anything but exactly 64 hex
/// characters — a malformed header hash is a malformed header.
fn hex_decode(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    // Lowercase only — the `BadHeader` contract is "64 lowercase-hex
    // characters", matching `hex_encode`'s output exactly.
    let nib = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            _ => None,
        }
    };
    let mut out = [0u8; 32];
    for (i, pair) in b.chunks_exact(2).enumerate() {
        out[i] = (nib(pair[0])? << 4) | nib(pair[1])?;
    }
    Some(out)
}

/// DEFLATE (zlib framing) over the canonical-CBOR encoding of `v` —
/// the §7.2 body pipeline, shared by encode and the versioned decode's
/// re-encode check.
fn compress_body<T: Serialize>(v: &T) -> Result<Vec<u8>, SaveError> {
    let mut cbor = Vec::new();
    ciborium::ser::into_writer(v, &mut cbor).map_err(SaveError::BodyEncode)?;
    Ok(miniz_oxide::deflate::compress_to_vec_zlib(
        &cbor,
        DEFLATE_LEVEL,
    ))
}

/// Encode [`Save`] as a complete §7.2 save file (magic + JSON header +
/// DEFLATE-over-CBOR body) at [`SAVE_VERSION_CURRENT`].
pub fn encode_save(save: &Save) -> Result<Vec<u8>, SaveError> {
    let body = SaveBodyV3 {
        snapshot: save.snapshot.clone(),
    };
    let compressed = compress_body(&body)?;
    let header = SaveHeader {
        save_version: SAVE_VERSION_CURRENT,
        game_version: save.game_version.clone(),
        hash_schema_version: csp_core::HASH_SCHEMA_VERSION,
        kdf_version: csp_core::KDF_VERSION,
        data_hash_version: csp_data::DATA_HASH_VERSION,
        data_set_hash: save.data_set_hash.as_ref().map(hex_encode),
        label: save.label.clone(),
        tick: Some(save.snapshot.clock_ticks),
        state_hash: save.state_hash.as_ref().map(hex_encode),
        compression: COMPRESSION_DEFLATE.into(),
        body_format: BODY_FORMAT_CBOR.into(),
        body_len: compressed.len() as u64,
    };
    let header_json = serde_json::to_vec(&header).map_err(SaveError::HeaderEncode)?;
    let mut out = Vec::with_capacity(SAVE_MAGIC.len() + 4 + header_json.len() + compressed.len());
    out.extend_from_slice(SAVE_MAGIC);
    out.extend_from_slice(&(header_json.len() as u32).to_le_bytes());
    out.extend_from_slice(&header_json);
    out.extend_from_slice(&compressed);
    Ok(out)
}

/// Decode a complete save file and migrate it to the current schema —
/// the §7.7 chain entry point. Refuses (never half-loads) on foreign
/// magic, a malformed header, an unmigratable version, an unknown
/// codec, or a body-framing mismatch.
pub fn decode_save(bytes: &[u8]) -> Result<Save, SaveError> {
    let (header, body) = parse_header(bytes)?;
    if header.compression != COMPRESSION_DEFLATE || header.body_format != BODY_FORMAT_CBOR {
        return Err(SaveError::UnsupportedEncoding {
            compression: header.compression,
            body_format: header.body_format,
        });
    }
    // Compare in `u64` — `body_len as usize` would truncate on wasm32
    // (a header declaring `2³² + body.len()` would pass the check
    // there). `parse_header`'s foreign-input convention: no usize
    // arithmetic on file-controlled numbers.
    if header.body_len != body.len() as u64 {
        return Err(SaveError::TrailingBytes);
    }
    let inflated = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(body, MAX_INFLATED_BODY)
        .map_err(SaveError::Inflate)?;
    let mut save = to_current(decode_body(header.save_version, &inflated)?);
    save.game_version = header.game_version;
    save.label = header.label;
    save.data_set_hash = match &header.data_set_hash {
        Some(h) => Some(hex_decode(h).ok_or(SaveError::BadHeader)?),
        None => None,
    };
    save.state_hash = match &header.state_hash {
        Some(h) => Some(hex_decode(h).ok_or(SaveError::BadHeader)?),
        None => None,
    };
    Ok(save)
}

/// The plaintext-header fields a slot listing needs — what
/// [`inspect_save`] returns without inflating or decoding the body.
/// `tick`/`state_hash` are `Option`: headers written before the
/// fields existed (and v1 files) carry neither.
#[derive(Clone, Debug, PartialEq)]
pub struct SaveMeta {
    /// The `save_version` chain key the body decodes as.
    pub save_version: u32,
    /// The writer's `game_version` string.
    pub game_version: String,
    /// The save slot's display label.
    pub label: Option<String>,
    /// The §3.1 data-set hash the save was written against.
    pub data_set_hash: Option<[u8; 32]>,
    /// The world's §7.1 state hash at save time.
    pub state_hash: Option<[u8; 32]>,
    /// The snapshot's `clock_ticks` at save time.
    pub tick: Option<u64>,
    /// The `hash_schema_version` the file was written under.
    pub hash_schema_version: u32,
}

/// Read the plaintext JSON header only — magic + `header_len` +
/// JSON, no body decode. The slot-listing/index path: O(header)
/// per save, never O(body). The same refusals [`decode_save`]
/// applies to the framing and to the hex hash fields it can see.
pub fn inspect_save(bytes: &[u8]) -> Result<SaveMeta, SaveError> {
    let (header, _) = parse_header(bytes)?;
    Ok(SaveMeta {
        save_version: header.save_version,
        game_version: header.game_version,
        label: header.label,
        data_set_hash: match &header.data_set_hash {
            Some(h) => Some(hex_decode(h).ok_or(SaveError::BadHeader)?),
            None => None,
        },
        state_hash: match &header.state_hash {
            Some(h) => Some(hex_decode(h).ok_or(SaveError::BadHeader)?),
            None => None,
        },
        tick: header.tick,
        hash_schema_version: header.hash_schema_version,
    })
}

/// Split `CSPSAVE\0 || header_len:u32 || JSON || body` apart.
fn parse_header(bytes: &[u8]) -> Result<(SaveHeader, &[u8]), SaveError> {
    let rest = bytes
        .strip_prefix(SAVE_MAGIC.as_slice())
        .ok_or(SaveError::BadMagic)?;
    let len_bytes = rest.get(..4).ok_or(SaveError::BadHeader)?;
    let header_len = u32::from_le_bytes(len_bytes.try_into().unwrap_or([0; 4])) as usize;
    // `get(..4)` above guarantees `len_bytes.len() == 4`; the unwrap_or
    // can only fire on a non-4 slice, which cannot happen — kept as a
    // total function rather than an unwrap per §2.13.
    //
    // `get(..header_len)` instead of `get(4..4 + header_len)`: on
    // wasm32 `4 + u32::MAX` overflows `usize` — a panic on the
    // foreign-input path is exactly what §2.13 forbids.
    let after_len = rest.get(4..).ok_or(SaveError::BadHeader)?;
    let header_bytes = after_len.get(..header_len).ok_or(SaveError::BadHeader)?;
    let header: SaveHeader =
        serde_json::from_slice(header_bytes).map_err(SaveError::HeaderDecode)?;
    Ok((header, &after_len[header_len..]))
}

/// Decode the inflated body under the schema `version` names — one arm
/// per frozen version, the §7.7 dispatch table.
fn decode_body(version: u32, bytes: &[u8]) -> Result<VersionedBody, SaveError> {
    /// CBOR decode with an exact-consumption proof: re-encoding the
    /// decoded value must reproduce the input bytes verbatim — one
    /// comparison that catches trailing garbage, truncation, and
    /// non-canonical encodings (our own writer is the reference for
    /// "canonical", so genuine saves always pass).
    fn decode<T>(bytes: &[u8]) -> Result<T, SaveError>
    where
        T: serde::de::DeserializeOwned + Serialize,
    {
        let mut cursor = std::io::Cursor::new(bytes);
        let v: T = ciborium::de::from_reader(&mut cursor).map_err(SaveError::BodyDecode)?;
        let mut again = Vec::with_capacity(bytes.len());
        ciborium::ser::into_writer(&v, &mut again).map_err(SaveError::BodyEncode)?;
        if again != bytes {
            return Err(SaveError::TrailingBytes);
        }
        Ok(v)
    }
    match version {
        // Stamped-2 bodies decode under the v3 schema: post-`engine`
        // writers emit exactly this tree, so a clean decode is the
        // proof the file is what its stamp failed to say. A decode
        // failure means the pre-`engine` write — an *unresolvable*
        // schema, not an out-of-chain version: `UnresolvableSchema`
        // names that honestly (the `1` stamp needs no arm at all —
        // SAVE_VERSION_MIN is 2, so it lands on the generic refusal).
        2 => Ok(VersionedBody::V3(
            decode(bytes).map_err(|_| SaveError::UnresolvableSchema { found: 2 })?,
        )),
        3 => Ok(VersionedBody::V3(decode(bytes)?)),
        found => Err(SaveError::UnsupportedVersion { found }),
    }
}

/// Fold the migration chain until the current schema falls out —
/// recursively, so adding vN+1 requires only a new `migrate_vN_to_vN+1`
/// and one more dispatch arm; the fold shape never changes.
fn to_current(body: VersionedBody) -> Save {
    match body {
        VersionedBody::V3(body) => Save {
            // Filled in by `decode_save` from the JSON header.
            game_version: String::new(),
            label: None,
            data_set_hash: None,
            state_hash: None,
            snapshot: body.snapshot,
        },
    }
}

/// The bug-report bundle's command-log artifact (§2.14): still the §7.6
/// bincode codec — replay logs are version-scoped by T3, deliberately
/// outside the §7.2 self-describing container.
pub fn encode_command_log(log: &[(CommandKey, SimCommand)]) -> Result<Vec<u8>, SaveError> {
    bincode::serde::encode_to_vec(log, bincode_config()).map_err(SaveError::LogEncode)
}

/// Inverse of [`encode_command_log`].
pub fn decode_command_log(bytes: &[u8]) -> Result<Vec<(CommandKey, SimCommand)>, SaveError> {
    let (v, read) = bincode::serde::decode_from_slice::<Vec<(CommandKey, SimCommand)>, _>(
        bytes,
        bincode_config(),
    )
    .map_err(SaveError::LogDecode)?;
    if read != bytes.len() {
        return Err(SaveError::TrailingBytes);
    }
    Ok(v)
}

#[cfg(test)]
// §2.13's no-panic rule covers shipping fallible paths; tests signal
// failure by panicking, so unwrap/expect stays legal inside test code.
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{SourceClass, World};

    fn live_snapshot() -> WorldSnapshot {
        World::new()
            .snapshot()
            .expect("empty world is serializable")
    }

    /// Build a save file under the §7.2 container at an arbitrary
    /// `save_version` — the fixture generator for historical-version
    /// tests. The header omits `data_set_hash`/`label` on v1 (the
    /// fields did not exist yet) to exercise the `Option` defaulting.
    fn fixture_bytes<T: Serialize>(save_version: u32, body: &T, game_version: &str) -> Vec<u8> {
        let compressed = compress_body(body).unwrap();
        let header = serde_json::json!({
            "save_version": save_version,
            "game_version": game_version,
            "hash_schema_version": csp_core::HASH_SCHEMA_VERSION,
            "kdf_version": csp_core::KDF_VERSION,
            "data_hash_version": csp_data::DATA_HASH_VERSION,
            "compression": COMPRESSION_DEFLATE,
            "body_format": BODY_FORMAT_CBOR,
            "body_len": compressed.len(),
        });
        let header_json = serde_json::to_vec(&header).unwrap();
        let mut out = Vec::new();
        out.extend_from_slice(SAVE_MAGIC);
        out.extend_from_slice(&(header_json.len() as u32).to_le_bytes());
        out.extend_from_slice(&header_json);
        out.extend_from_slice(&compressed);
        out
    }

    /// A stamped-N file whose body IS the current schema — the
    /// "wrong stamp, right bytes" fixture.
    fn stamped_bytes(version: u32, snapshot: &WorldSnapshot, game_version: &str) -> Vec<u8> {
        fixture_bytes(
            version,
            &SaveBodyV3 {
                snapshot: snapshot.clone(),
            },
            game_version,
        )
    }

    #[test]
    fn save_round_trip_current() {
        let save = Save {
            game_version: "0.0.0".into(),
            label: Some("first flight".into()),
            data_set_hash: Some([0xAB; 32]),
            state_hash: Some([0xCD; 32]),
            snapshot: live_snapshot(),
        };
        let bytes = encode_save(&save).unwrap();
        assert!(bytes.starts_with(SAVE_MAGIC));
        let back = decode_save(&bytes).unwrap();
        assert_eq!(back, save);
        // The slot-listing read: `inspect_save` sees every envelope
        // field without touching the body.
        let meta = inspect_save(&bytes).unwrap();
        assert_eq!(meta.save_version, SAVE_VERSION_CURRENT);
        assert_eq!(meta.label.as_deref(), Some("first flight"));
        assert_eq!(meta.data_set_hash, Some([0xAB; 32]));
        assert_eq!(meta.state_hash, Some([0xCD; 32]));
        assert_eq!(meta.tick, Some(save.snapshot.clock_ticks));
    }

    /// §7.2 byte-stability: encoding the same `Save` twice produces the
    /// same file — the canonical-CBOR body + pinned DEFLATE level make
    /// the container deterministic end to end.
    #[test]
    fn save_encoding_is_byte_stable() {
        let save = Save {
            game_version: "0.0.0".into(),
            label: Some("stable".into()),
            data_set_hash: Some([0x01; 32]),
            state_hash: None,
            snapshot: live_snapshot(),
        };
        assert_eq!(encode_save(&save).unwrap(), encode_save(&save).unwrap());
    }

    /// The header is what §7.2 promises: plaintext JSON carrying every
    /// fingerprint field, parseable without touching the body.
    #[test]
    fn header_is_self_describing_json() {
        let save = Save {
            game_version: "1.2.3".into(),
            label: Some("x".into()),
            data_set_hash: Some([0x5A; 32]),
            state_hash: None,
            snapshot: live_snapshot(),
        };
        let bytes = encode_save(&save).unwrap();
        let len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let header: serde_json::Value = serde_json::from_slice(&bytes[12..12 + len]).unwrap();
        assert_eq!(header["save_version"], SAVE_VERSION_CURRENT);
        assert_eq!(header["game_version"], "1.2.3");
        assert_eq!(header["hash_schema_version"], csp_core::HASH_SCHEMA_VERSION);
        assert_eq!(header["kdf_version"], csp_core::KDF_VERSION);
        assert_eq!(header["data_hash_version"], csp_data::DATA_HASH_VERSION);
        assert_eq!(
            header["data_set_hash"],
            "5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a"
        );
        assert_eq!(header["compression"], "deflate");
        assert_eq!(header["body_format"], "cbor");
        assert_eq!(
            header["body_len"].as_u64().unwrap() as usize,
            bytes.len() - 12 - len
        );
    }

    #[test]
    fn unlabeled_save_round_trips() {
        let save = Save {
            game_version: "0.0.0".into(),
            label: None,
            data_set_hash: None,
            state_hash: None,
            snapshot: live_snapshot(),
        };
        assert_eq!(decode_save(&encode_save(&save).unwrap()).unwrap(), save);
    }

    /// C8-F3 institutionalized: a stamped-1 file is refused
    /// `UnsupportedVersion` — the honest answer, since the v1 snapshot
    /// was never frozen into real v1 types (it aliased the live tree,
    /// so no genuine first-public-build body can decode under any
    /// schema this build names). The message must be TRUE: `1` is
    /// outside the readable chain `2..=3` — not the old lie that named
    /// it "outside 1..=3" while claiming to support it. Even a body
    /// that happens to BE the current schema is refused — the stamp,
    /// not the payload, is the contract.
    #[test]
    fn stamped_v1_refused_as_outside_the_chain() {
        let bytes = stamped_bytes(1, &live_snapshot(), "1.0.0-public");
        match decode_save(&bytes) {
            Err(SaveError::UnsupportedVersion { found }) => {
                assert_eq!(found, 1);
                let msg = format!("{}", SaveError::UnsupportedVersion { found });
                assert!(
                    msg.contains("outside this build's chain (2..=3)"),
                    "honest message, got: {msg}"
                );
            }
            other => panic!("expected UnsupportedVersion(1), got {other:?}"),
        }
    }

    /// A stamped-2 body carrying the *current* schema (a post-`engine`
    /// writer's save — the stamp lagged the schema, the bytes did not)
    /// still loads: the v2 arm decodes under the shared v3 body.
    #[test]
    fn stamped_v2_current_schema_loads() {
        let snapshot = live_snapshot();
        let body = SaveBodyV3 {
            snapshot: snapshot.clone(),
        };
        let bytes = fixture_bytes(2, &body, "2.0.0-post-engine");
        let save = decode_save(&bytes).unwrap();
        assert_eq!(save.snapshot, snapshot);
    }

    /// A stamped-2 body that cannot prove the current schema — the
    /// pre-`WeldPart::engine` write — refuses `UnresolvableSchema`:
    /// the stamp IS inside the chain, so "outside the chain" would be
    /// a lie; the body simply matches no schema this build can name
    /// (and never the silent `engine: None` decode that unmetered
    /// every resolved engine).
    #[test]
    fn stamped_v2_unresolvable_body_refused_as_version() {
        let bytes = fixture_bytes(2, &42u64, "1.x-pre-engine");
        match decode_save(&bytes) {
            Err(SaveError::UnresolvableSchema { found }) => assert_eq!(found, 2),
            other => panic!("expected UnresolvableSchema(2), got {other:?}"),
        }
    }

    /// §7.7: a save outside the chain is refused with the versions named,
    /// never half-loaded.
    #[test]
    fn unsupported_version_refused() {
        let bytes = stamped_bytes(99, &live_snapshot(), "9.9.9");
        match decode_save(&bytes) {
            Err(SaveError::UnsupportedVersion { found }) => assert_eq!(found, 99),
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
    }

    /// A header naming a codec this build does not speak is refused by
    /// name — never silently probed.
    #[test]
    fn unknown_codec_refused() {
        let snapshot = live_snapshot();
        let mut bytes = stamped_bytes(3, &snapshot, "3.0.0");
        // Rewrite the header with body_format "msgpack".
        let len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let mut header: serde_json::Value = serde_json::from_slice(&bytes[12..12 + len]).unwrap();
        header["body_format"] = "msgpack".into();
        let header_json = serde_json::to_vec(&header).unwrap();
        let mut bad = Vec::new();
        bad.extend_from_slice(SAVE_MAGIC);
        bad.extend_from_slice(&(header_json.len() as u32).to_le_bytes());
        bad.extend_from_slice(&header_json);
        bad.extend_from_slice(&bytes[12 + len..]);
        bytes.clear();
        assert!(matches!(
            decode_save(&bad),
            Err(SaveError::UnsupportedEncoding { .. })
        ));
    }

    #[test]
    fn foreign_bytes_refused() {
        assert!(matches!(
            decode_save(b"NOTCSPSAVE blah blah"),
            Err(SaveError::BadMagic)
        ));
        assert!(matches!(decode_save(b""), Err(SaveError::BadMagic)));
        // Magic present but the framing fields are missing.
        assert!(matches!(
            decode_save(b"CSPSAVE\0"),
            Err(SaveError::BadHeader)
        ));
        // Header_len overruns the file.
        let mut overrun = SAVE_MAGIC.to_vec();
        overrun.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(decode_save(&overrun), Err(SaveError::BadHeader)));
        // Header_len present but the JSON is not.
        let mut bad_json = SAVE_MAGIC.to_vec();
        bad_json.extend_from_slice(&3u32.to_le_bytes());
        bad_json.extend_from_slice(b"xyz");
        assert!(matches!(
            decode_save(&bad_json),
            Err(SaveError::HeaderDecode(_))
        ));
    }

    #[test]
    fn trailing_bytes_refused() {
        let save = Save {
            game_version: "0.0.0".into(),
            label: None,
            data_set_hash: None,
            state_hash: None,
            snapshot: live_snapshot(),
        };
        let mut bytes = encode_save(&save).unwrap();
        bytes.push(0xFF);
        assert!(matches!(decode_save(&bytes), Err(SaveError::TrailingBytes)));
    }

    #[test]
    fn command_log_round_trip() {
        let mut world = World::new();
        world.enqueue(SourceClass::Ui, 0, SimCommand::SetWarp(4));
        world.enqueue(SourceClass::Ui, 0, SimCommand::AdvanceTo(64));
        world.step_frame(64).unwrap();
        let encoded = encode_command_log(world.command_log()).unwrap();
        let decoded = decode_command_log(&encoded).unwrap();
        assert_eq!(decoded, world.command_log());
    }
}
