# ATTRIBUTION (PLAN §3.7)
#
# One pipe-delimited line per **non-authored** asset, anywhere under data/
# (recursive — all subdirs are walked):
#
#     path | source | license | url
#
# License must be on the §3.7 allowlist: CC0-1.0, public-domain, MIT,
# Apache-2.0, BSD-2-Clause, BSD-3-Clause, Zlib, BSL-1.0, OFL-1.1,
# Unlicense, CC-BY-3.0, CC-BY-4.0. Never CC-BY-NC, CC-BY-SA, or GPL —
# §1.2 makes the game proprietary, so NC and share-alike are unusable.
#
# Own-authored files need no line. `xtask lint-attribution` reads the
# authored sets declared below; matching is by filename SUFFIX, so
# compound suffixes must be spelled out in full (`.ron.disabled`, not
# `.ron`):
# authored-extensions: .ron .md .ron.disabled .txt .json .bin
# authored-names: .gitkeep LICENSE ATTRIBUTION.md biomes.png
#
# `biomes.png` is declared authored by NAME (§3.0.1: biome maps are
# hand-authored fiction, generated reproducibly by
# `cargo run -p xtask --bin gen_biome_maps`). Other PNGs (textures etc.)
# still need attribution rows.
#
# Every other line in this file is either a '#' comment like these, or a
# 4-field attribution row. Rows belong below:
#
# path | source | license | url
data/art/parts.csar | procedural rocket-part meshes authored in xtask/src/bin/parts.rs, baked by xtask bake_art | MIT | https://github.com/DimitriGilbert/Clanker-Space-Program
data/art/terrain.csar | ambientCG Grass003/Rock035/Ground080/Snow010A/Foam002 PBR maps, baked by xtask bake_art | CC0-1.0 | https://ambientcg.com
assets/src/Grass003/Grass003_1K-JPG_Color.jpg | ambientCG Grass003 (grass) | CC0-1.0 | https://ambientcg.com/view?id=Grass003
assets/src/Grass003/Grass003_1K-JPG_NormalGL.jpg | ambientCG Grass003 (grass) | CC0-1.0 | https://ambientcg.com/view?id=Grass003
assets/src/Rock035/Rock035_1K-JPG_Color.jpg | ambientCG Rock035 (rock) | CC0-1.0 | https://ambientcg.com/view?id=Rock035
assets/src/Rock035/Rock035_1K-JPG_NormalGL.jpg | ambientCG Rock035 (rock) | CC0-1.0 | https://ambientcg.com/view?id=Rock035
assets/src/Ground080/Ground080_1K-JPG_Color.jpg | ambientCG Ground080 (sand) | CC0-1.0 | https://ambientcg.com/view?id=Ground080
assets/src/Ground080/Ground080_1K-JPG_NormalGL.jpg | ambientCG Ground080 (sand) | CC0-1.0 | https://ambientcg.com/view?id=Ground080
assets/src/Snow010A/Snow010A_1K-JPG_Color.jpg | ambientCG Snow010A (snow) | CC0-1.0 | https://ambientcg.com/view?id=Snow010A
assets/src/Snow010A/Snow010A_1K-JPG_NormalGL.jpg | ambientCG Snow010A (snow) | CC0-1.0 | https://ambientcg.com/view?id=Snow010A
assets/src/Foam002/Foam002_1K-JPG_Color.jpg | ambientCG Foam002 (ocean-foam) | CC0-1.0 | https://ambientcg.com/view?id=Foam002
assets/src/Foam002/Foam002_1K-JPG_NormalGL.jpg | ambientCG Foam002 (ocean-foam) | CC0-1.0 | https://ambientcg.com/view?id=Foam002
