# Art assets — sources & licenses

The bake `cargo run -p xtask --bin bake_art`
(`xtask/src/bin/bake_art.rs`) packs `data/art/*.csar`, decoded at
startup by `csp_view::art`. Nothing under `assets/` is read at
runtime — it is the reproducible bake input.

## Rocket parts — procedurally authored, no external source

The part meshes in `data/art/parts.csar` are **authored in code** by
`xtask/src/bin/parts.rs`: real aerospace geometry built as
lathe/sweep/prism meshes (Rao-flared double-wall bell nozzles with
corrugated cooling channels and copper liners, an Apollo-class
capsule with recessed windows and a docking probe, tanks with
ellipsoidal domes and raised weld rings, a tangent-ogive nosecone,
an SRB, machined rings, a superellipse battery pack, an instrument
canister, a fin can). No third-party model files are bake inputs —
the Kenney Space Kit GLBs were retired because stylized cartoon
primitives cannot bake into real hardware.

## Terrain textures (ambientCG, CC0)

| Source file(s) (`assets/src/…`) | Origin | License | URL |
|---|---|---|---|
| `Grass003/…_Color.jpg`, `…_NormalGL.jpg` | ambientCG "Grass 003" | CC0-1.0 | https://ambientcg.com/view?id=Grass003 |
| `Rock035/…_Color.jpg`, `…_NormalGL.jpg` | ambientCG "Rock 035" | CC0-1.0 | https://ambientcg.com/view?id=Rock035 |
| `Ground080/…_Color.jpg`, `…_NormalGL.jpg` | ambientCG "Ground 080" (beach sand) | CC0-1.0 | https://ambientcg.com/view?id=Ground080 |
| `Snow010A/…_Color.jpg`, `…_NormalGL.jpg` | ambientCG "Snow 010 A" | CC0-1.0 | https://ambientcg.com/view?id=Snow010A |
| `Foam002/…_NormalGL.jpg` | ambientCG "Foam 002" (ocean foam → wave normals) | CC0-1.0 | https://ambientcg.com/view?id=Foam002 |

Attribution is not legally required (CC0) but given anyway — thanks
ambientCG.
