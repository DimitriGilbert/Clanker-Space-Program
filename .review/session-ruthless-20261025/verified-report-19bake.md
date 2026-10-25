# Verified Report 19b — xtask/src/bin/bake_art.rs

Verifier: static re-verification against working tree at HEAD 1e8c6da
(git status: only .review/ files modified — no concurrent edits to
bake_art.rs, art.rs, terrain.rs, or terrain.wgsl; nothing to mark
FIXED-BY-CONCURRENT-EDIT).

### Finding 1: Normal-map mip chain averages normals without renormalizing (and Lanczos-resizes them) — CONFIRMED
**Original**: `downsample` box-filters RGBA identically for normal layers; sources Lanczos3-resized; terrain.wgsl uses `.xy*2-1` as slope un-renormalized, so distant terrain flattens.
**Verification**:
- xtask/src/bin/bake_art.rs:461-487 — `downsample` accumulates 4 channels as plain u32 sums and emits `(acc[c] / cnt) as u8` (line 482). There is no per-layer-type flag and no renormalization path anywhere in the file; `image_entry` (491-534) applies the same chain to `nrm_layers` (569-577 → entry at 588) and `water` (578 → entry at 592).
- xtask/src/bin/bake_art.rs:456 — sources go through `resize_exact(w, h, FilterType::Lanczos3)` before the mip chain; Lanczos negative lobes ring on NormalGL edges, and the CPU-side filtering of albedo also happens on gamma-encoded bytes (upload format is `Rgba8UnormSrgb`, terrain.rs:1278-1280, so only GPU sampling is linear-correct — the CPU mip chain is not).
- Consumer 1 (terrain classes): terrain.wgsl:501-505 `class_normal` does `textureSample(nrm_arr, ...).xy * 2.0 - 1.0` with no renormalize; terrain.wgsl:697-698 then perturbs with `n_fix = normalize(n_fix - basis.e * ntex.x * 0.55 - basis.n * ntex.y * 0.55)` — the final `normalize` of the whole vector cannot restore the magnitude lost from `ntex.xy`; shortened mip normals scale the perturbation angle down directly. Sampler is `mipmap_filter: Linear` (terrain.rs:1075), so shortened higher mips blend in from mid-distance, not just at the level swap.
- Consumer 2 (water): terrain.wgsl:602-609 — `water_nrm` sampled the same way (`.xy * 2.0 - 1.0` into `wg`), slope feeds `normalize(dir_fix - basis.e * wg.x - basis.n * wg.y)` at 610. Same flattening at distance.
- Math holds: the mean of unit vectors is sub-unit (strictly, unless all equal), so every level >0 stores shortened normals; for noisy maps (Grass003 NormalGL) the shortfall is material within a few levels. This is the standard reason normal-map mips need renormalization; nothing here does it.

### Finding 2: Per-mip floor-truncation biases the shader's mean; header `means` is dead and disagrees with it — CONFIRMED
**Original**: `(acc/cnt) as u8` floors every mip level; the shader derives its mean from the deepest mip while the header `means` (level-0 based) is read by nothing — two disagreeing sources of truth.
**Verification**:
- xtask/src/bin/bake_art.rs:482 — integer division floors each downsample step (up to 9 steps for T=512: 512→1, loop at 510-525), always downward, ≤1 LSB per level per channel.
- xtask/src/bin/bake_art.rs:493-505 — `means` is the exact level-0 integer mean (floored once).
- terrain.wgsl:495-497 — the consumed mean is `textureLoad(alb_arr, vec2<i32>(0,0), layer, last).rgb` with `last = textureNumLevels(alb_arr) - 1`, i.e. the 9-times-floored 1x1 mip, not the header field. Net bias: detail runs bright by up to ~(9−l)/255 at level l.
- Dead-field check: repo-wide grep for `means` — the only occurrences are the baker (bake_art.rs:493), the decode-time store (crates/csp-view/src/art.rs:227-239), and the struct field (crates/csp-render/src/terrain.rs:536). No read anywhere in crates/. Worse than reported: the field's own doc (terrain.rs:534-535) claims "the shader normalizes texel detail against these" — false; and the format doc (bake_art.rs:10-11) advertises "per-layer average" as if it were load-bearing. Meanwhile terrain.wgsl:482-483's comment ("the deepest mip IS the mean") shows the shader deliberately uses the other value. Two sources of truth, one dead, docs on both sides pointing at the wrong one.

### Finding 3: Packs written non-atomically and out of dependency order — CONFIRMED
**Original**: parts.csar is committed before terrain sources decode; a corrupt jpg panics after parts.csar was replaced, leaving fresh parts + stale terrain silently embedded by include_bytes.
**Verification**:
- xtask/src/bin/bake_art.rs:551 — `write_csar(&out.join("parts.csar"), ...)` runs after `parts::build_all()` but strictly before the four `acg(...)` albedo loads (560-568), four normal loads (569-577), and the Foam002 load (578).
- xtask/src/bin/bake_art.rs:454-455 — `load_rgba8` panics on any decode failure (`unwrap_or_else(|e| panic!("decode ..."))`), so the process dies mid-bake with parts.csar already replaced and terrain.csar untouched.
- xtask/src/bin/bake_art.rs:151 — `write_csar` ends in a plain `fs::write` (no tmp+rename), so a mid-write kill additionally leaves a truncated file.
- crates/csp-view/src/art.rs:86-87 — both packs are `include_bytes!`-embedded, and both are git-tracked (`git ls-files data/art/` lists parts.csar and terrain.csar): the next `cargo build` bakes the mixed generation into the binary with no error, and the mixed pair can be committed. Confirmed silent-failure mode.

### Finding 4: No payload size/checksum in image entries — bad pack panics at first texture upload — CONFIRMED
**Original**: image entries record only the compressed length; the loader inflates and trusts, so w/h/layers/mips vs payload disagreement survives decode and panics in write_art_tex despite the "never fatal" contract.
**Verification**:
- xtask/src/bin/bake_art.rs:125-127 — image entry writes only `(comp.len() as u64).to_le_bytes()` then the deflate bytes; no raw length, no CRC. (Mesh branch identical at 145-147, but see below.)
- crates/csp-view/src/art.rs:224-242 — image branch reads dims/means and inflates with zero cross-checks; contrast the mesh branch's explicit `raw.len() != nv * 32 + ni * 4` rejection at art.rs:247. The asymmetry is real.
- crates/csp-render/src/terrain.rs:1228 and 1233-1235 — `tex.data[src..src + level_bytes]` / `tex.data[s..s + row]` panic (slice out of range) whenever the inflated payload is shorter than `layers * sum(max(1,w>>l) * max(1,h>>l) * 4)`. `level_bytes` comes from the header dims (1217-1220), so header/payload disagreement panics exactly as claimed.
- Call path: `write_art_tex` (1201) ← `upload_art` (1267, calls at 1282-1284) ← `draw` (1380) — first rendered frame, i.e. effectively startup, killing the process. This contradicts the loader's contract at art.rs:89-91 ("never fatal") and terrain.rs's own cursor design ("a corrupt asset degrades, it doesn't kill", art.rs:157-158).
- Nuance: miniz's zlib path does verify adler32, so random mid-stream corruption usually fails inflate and degrades gracefully; the realistic triggers are dims-header corruption (w/h/layers inflated with intact payload) or any future bake/layout drift that changes payload size without changing dims — which is precisely the regression the mesh-side check exists to catch. Gap confirmed; the format is the right place to fix it.

### Finding 5: Stale Loop-subdivision docs — CONFIRMED
**Original**: three comments/docs still describe a Loop-subdivision stage that no longer exists.
**Verification**:
- xtask/src/bin/bake_art.rs:178-179 — "are a crease — Loop sharp-rules it and the normal rebuild splits it. Kenney's 45° bevels stay under the bar" (both the Loop reference and the Kenney reference are stale — the Kenney kit was retired, 445-447).
- xtask/src/bin/bake_art.rs:253-256 — "Applied to the coarse mesh BEFORE subdivision so the falloff spreads smoothly through the subdivided descendants" — there is no subdivision; the comment directly contradicts bake_art.rs:162-165 ("no subdivision step is needed").
- crates/csp-view/src/art.rs:20-21 — "Meshes are the bake's welded, Loop-subdivided, smoothing-wedge-normalled models".
- Additional instance beyond the three cited: `finish`'s docstring at bake_art.rs:316-317 says "Rebuild the emitted mesh from the subdivided working mesh". Same class of staleness; fix alongside.

## Determinism spot-check — CLAIM HOLDS
- xtask/src/bin/bake_art.rs imports only `BTreeMap` (line 58; usages 235, 400). Grep for `HashMap|rand|SystemTime|Instant|now()` over bake_art.rs and xtask/src/bin/parts/mod.rs: zero matches.
- Compression via `miniz_oxide` (deterministic), resize via `image` (deterministic), single-threaded f64 mesh math, no wall-clock or RNG input anywhere in the bake path. Byte-identical rebakes for a given toolchain/platform: supported. (Cross-rustc-version bit-reproducibility of f64/f32 casts is not asserted by anything, but no in-repo hazard exists.)

## Summary
5 confirmed, 0 dismissed, 0 fixed-by-concurrent-edit (no source files touched since HEAD 1e8c6da; all findings still present in the working tree).
