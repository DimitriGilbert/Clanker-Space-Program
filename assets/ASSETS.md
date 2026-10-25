# Art assets — sources & licenses

All third-party art is CC0 (public-domain dedication); every file is
baked into `data/art/*.csar` packs by `cargo run -p xtask --bin bake_art`
(`xtask/src/bin/bake_art.rs`) and decoded at startup by
`csp_view::art`. Nothing under `assets/` is read at runtime — it is the
reproducible bake input. Rows below are also recorded in
`data/ATTRIBUTION.md` for the shipped `data/art/` files.

| Source file(s) (`assets/src/…`) | Origin | License | URL |
|---|---|---|---|
| `kenney_space-kit/Models/GLTF format/rocket_baseA.glb`, `rocket_baseB.glb`, `rocket_topA.glb`, `rocket_topB.glb`, `rocket_fuelB.glb`, `rocket_sidesA.glb`, `rocket_finsA.glb`, `pipe_ring.glb`, `structure.glb`, `machine_wireless.glb`, `machine_barrel.glb` | Kenney "Space Kit" 2.0 | CC0-1.0 | https://kenney.nl/assets/space-kit |
| `Grass003/…_Color.jpg`, `…_NormalGL.jpg` | ambientCG "Grass 003" | CC0-1.0 | https://ambientcg.com/view?id=Grass003 |
| `Rock035/…_Color.jpg`, `…_NormalGL.jpg` | ambientCG "Rock 035" | CC0-1.0 | https://ambientcg.com/view?id=Rock035 |
| `Ground080/…_Color.jpg`, `…_NormalGL.jpg` | ambientCG "Ground 080" (beach sand) | CC0-1.0 | https://ambientcg.com/view?id=Ground080 |
| `Snow010A/…_Color.jpg`, `…_NormalGL.jpg` | ambientCG "Snow 010 A" | CC0-1.0 | https://ambientcg.com/view?id=Snow010A |
| `Foam002/…_NormalGL.jpg` | ambientCG "Foam 002" (ocean foam → wave normals) | CC0-1.0 | https://ambientcg.com/view?id=Foam002 |
| `lakeside_night_2k.hdr` | Poly Haven "Lakeside Night" HDRI (equirect star field, tone-mapped at bake) | CC0-1.0 | https://polyhaven.com/a/lakeside_night |

Attribution is not legally required (CC0) but given anyway — thanks
Kenney, ambientCG, and Poly Haven.
