# Review 19: xtask/src/bin/bake_art.rs (GLB -> .csar v2 art baker)

Static review, no cargo run. Findings appended below as confirmed.

Scope note: the file is 596 lines and no longer contains any GLB parsing — the Kenney GLB path was retired (bake_art.rs:445-447, parts are procedural via parts/mod.rs). "GLB robustness" items are moot; review covered the real surface: csar write, weld/edge-shade/wedge-normal pass, mip generation, determinism, output atomicity, and the cross-crate contract with crates/csp-view/src/art.rs + crates/csp-render/src/terrain.rs/terrain.wgsl.

### [SEVERITY: MEDIUM] Finding 1: Normal-map mip chain averages normals without renormalizing (and Lanczos-resizes them)
**File**: xtask/src/bin/bake_art.rs:461-487 (downsample), 453-459 (load_rgba8), 569-578
**Problem**: `downsample` box-averages RGBA bytes for every layer type. Normal maps are linear data: the mean of unit normals is not unit length. Nothing renormalizes; every mip level >0 stores shortened, blue-up-biased normals. Sources are additionally resized with `FilterType::Lanczos3` (line 456), whose negative lobes ring on the sharp edges typical of NormalGL maps.
**Evidence**: `out[o + c] = (acc[c] / cnt) as u8;` treats channels identically for the "normal" stack (lines 569-577) and `water_nrm` (578); consumer terrain.wgsl:503 does `textureSample(nrm_arr,...).xy * 2.0 - 1.0` and uses the result directly as slope — no renormalize — so the stored shortening scales distant-terrain relief down.
**Impact**: For noisy maps (Grass003), the 2x2 mean can drop well below unit length within a few levels; mip-filtered distant terrain systematically flattens (weaker slope shading), and Lanczos ringing adds halos. Albedo layers are also filtered in gamma space (classic darkening), though the shader's divide-by-deepest-mip normalization (terrain.wgsl:496-497) cancels most of that bias — the normal-map loss has no such correction.
**Suggestion**: Give `image_entry`/`downsample` a `linear: bool` (or `renormalize`) flag; for normal layers renormalize `(r-?,g-?,b)` per averaged texel before quantize (and z=255 convention accordingly), and resize normals with `FilterType::CatmullRom` or `Triangle` instead of Lanczos3.

### [SEVERITY: MEDIUM] Finding 2: Per-mip floor-truncation biases the shader's "mean"; header `means` field is dead and disagrees with it
**File**: xtask/src/bin/bake_art.rs:482, 493-505; consumer terrain.wgsl:495-497
**Problem**: Every downsample level floors: `(acc[c] / cnt) as u8` — up to 1 LSB dark per level, 9 levels for T=512, always in the same direction. terrain.wgsl derives its per-layer mean from the deepest mip (`textureLoad(alb_arr, vec2<i32>(0,0), layer, last)`), not from the `means` field this bake computes from level 0. The two values can differ by up to ~9 LSB (~3.5%), and `means` is read by nothing: verified — `tex.means` is stored (terrain.rs:536) and never used again anywhere in crates/.
**Evidence**: bake writes `means` as the exact level-0 integer mean (493-505) while the actually-consumed mean is the 9-times-floored 1x1 mip. The module doc (lines 10-14) advertises "per-layer average" as if it were the consumer's mean. Nothing on either side asserts deepest-mip == header mean — this is exactly the unenforced cross-crate invariant.
**Impact**: Normalized albedo detail runs systematically ~1-3% bright (texels carry less accumulated truncation than the 1x1 divisor); 24 dead bytes per image in the pack; the next consumer that trusts the documented `means` field gets a value that disagrees with what the shader uses.
**Suggestion**: Round at each level (`(acc[c] + cnt / 2) / cnt`); then either delete `means` from the format (bump version per the doc's own rule) or make the shader use it — one source of truth.

### [SEVERITY: MEDIUM] Finding 3: Packs written non-atomically and out of dependency order — partial bake failure leaves mixed-generation artifacts
**File**: xtask/src/bin/bake_art.rs:551 (parts write), 560-578 (source decode), 151 (fs::write)
**Problem**: `write_csar(parts.csar)` commits before the terrain sources are even decoded. If any ambientCG jpg is missing or corrupt, `load_rgba8` panics (line 455) after parts.csar was already replaced, leaving a fresh parts.csar beside a stale terrain.csar. `fs::write` is also not atomic — a mid-write kill leaves a truncated pack on disk.
**Evidence**: `write_csar(&out.join("parts.csar"), &entries);` at 551 precedes the four `acg(...)` loads at 560-578; both packs are `include_bytes!`-embedded (csp-view/src/art.rs:86-87), so the next `cargo build` bakes the mixed generation into the binary silently (the truncated-file case at least degrades to the warned fallback at load).
**Impact**: A failed bake masquerades as a successful one: new part models ship with old terrain textures (or vice versa) with no error anywhere in the build.
**Suggestion**: Assemble both packs' bytes fully in memory, then write each via `std::fs::write(tmp)` + `fs::rename` onto the final path only after every asset decoded — all-or-nothing across the two files.

### [SEVERITY: MEDIUM] Finding 4: Image entries carry no payload-size/checksum field — a bad pack passes decode and panics at first texture upload
**File**: xtask/src/bin/bake_art.rs:125-127 (writer side); the gap is cross-crate
**Problem**: The image entry header records only the compressed length (`comp.len() as u64`). The mesh branch's loader cross-checks `raw.len() == nv*32 + ni*4` (art.rs:247), but the image branch inflates and trusts (art.rs:231) — any pack whose w/h/layers/mips disagree with the inflated payload survives decode, then terrain.rs:1228 (`tex.data[src..src + level_bytes]`) panics on the first `write_art_tex`. This is the known loader gap; the root cause lives in the format this file defines.
**Evidence**: `out.extend_from_slice(&(comp.len() as u64).to_le_bytes());` — no raw-length or CRC. The doc itself (lines 52-53) commits to "bump version on any layout change", so adding a field is already sanctioned process.
**Impact**: Corruption or a future bake/layout bug converts a presentation asset into a startup crash instead of the documented graceful fallback (art.rs:90-91 promises "never fatal").
**Suggestion**: In the v3 entry header, write `u64 raw_len` (and ideally a CRC32 of the raw payload) next to the compressed length; have art.rs reject image entries whose inflated length != layers * sum(max(1,w>>l) * max(1,h>>l) * 4) before terrain.rs slices.

### [SEVERITY: LOW] Finding 5: Stale subdivision claims in docs/comments contradict the retired pipeline
**File**: xtask/src/bin/bake_art.rs:179 ("Loop sharp-rules it"), 254-255 ("BEFORE subdivision"), and crates/csp-view/src/art.rs:20-22 ("Loop-subdivided models")
**Problem**: The code no longer subdivides (bake_art.rs:163-165, 445-447), but three comments/docs still describe a Loop-subdivision stage, and the loader's format doc tells the next maintainer the meshes are "the bake's welded, Loop-subdivided, smoothing-wedge-normalled models".
**Impact**: Misleads anyone reasoning about vertex counts/creases or debugging shading into looking for a subdivision pass that does not exist.
**Suggestion**: Drop the Loop references from bake_art.rs:179 and 254-255 and from art.rs's doc header.

Checked and clean (no findings): determinism — BTreeMap only in bake_art.rs and parts/mod.rs (no HashMap/rand/time), single-threaded f64 math, miniz_oxide deterministic → byte-identical rebakes. Mip chain provably reaches 1x1 (lines 510-525, `(w/2).max(1)`), mips=10 for T=512 matches wgpu's mip_level_count bound. Wedge partition/weld/edge-shade: `position().unwrap()` at 355/381/393 cannot fail (vf[v] is built only from faces containing v); zero-area faces degrade to deterministic fallbacks, no panics found. write/read byte layouts match the loader (mesh 32 B vertex, nv*32+ni*4 checked at art.rs:247). Premultiplied-alpha concern is moot: all sources are JPG (alpha=255).

Verdict: REQUEST CHANGES.
No GLB parsing remains — that review axis is void. The bake itself is
deterministic and its mesh math is sound, but the mip path mishandles
normal maps (1), ships a dead + divergent means field (2), can leave
mixed-generation packs on failure (3), and defines a format that turns
a bad pack into a startup panic rather than the promised fallback (4).
1-3 are one-commit fixes; 4 is a version bump both sides already plan for.
