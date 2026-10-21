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
# authored-names: .gitkeep LICENSE ATTRIBUTION.md
#
# Every other line in this file is either a '#' comment like these, or a
# 4-field attribution row. Rows belong below:
#
# path | source | license | url
