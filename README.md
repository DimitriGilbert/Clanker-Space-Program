# Clanker Space Program

Clanker Space Program is a KSP-style (Kerbal Space Program) build-and-fly space simulation with a
persistent real-time strategy progression layer, set in a fictional hand-authored solar system. It is
a Rust workspace of 12 crates plus `xtask` (edition 2024) built on a standalone `bevy_ecs`, wgpu and
egui. The rules of the game run in an authoritative headless simulation built for determinism: a
fixed 64 Hz integer tick, floating-point transcendentals through `libm` only, a BLAKE3 state hash
and golden replays. The renderer and UI are a presentation layer on top of that simulation, and the
same simulation can be driven without a window over HTTP, WebSocket and MCP.

- [Status](#status)
- [Supported platforms](#supported-platforms)
- [Repository layout](#repository-layout)
- [Prerequisites and setup](#prerequisites-and-setup)
  - [Rust toolchain](#rust-toolchain)
  - [Linux](#linux): [Fedora](#fedora), [Ubuntu and Debian](#ubuntu-and-debian),
    [Arch Linux](#arch-linux)
  - [Windows](#windows)
  - [macOS](#macos)
- [Build and run](#build-and-run)
- [Tests and quality gates](#tests-and-quality-gates)
- [Web demo and telemetry web app](#web-demo-and-telemetry-web-app)
- [Content and art tooling](#content-and-art-tooling)
- [Troubleshooting](#troubleshooting)
- [Documentation index and licensing](#documentation-index-and-licensing)

## Status

`PLAN.md` §6 is the milestone plan, M0 to M10. Each row lists measurable exit criteria; the table
has no completion column, and this README therefore does not claim that any milestone is finished.

The per-feature state of the game is recorded in the "State of the game" table of
`docs/GAMEPLAY-GUIDE.md` §10, written against the tree on 2026-10-26. In summary:

- **Playable:** the VAB editor; launch as a paid build order; flight with HUD, navball and staging;
  map view; maneuver nodes with the burn executor; the transfer planner; parachute re-entry and
  vessel recovery; science; research (thin: 4 tech nodes, 5 tracks, 16 parts); contracts; funds and
  the economy; crew; labs; part manufacturing and bench testing; part failures and wear; timewarp;
  alarms; save, load, quicksave and revert; the Rhai scripting console with stock scripts;
  synthesized sound; the MCP server and telemetry web app for agents.
- **Partial:** colonies (view and logistics only, no colony design); autopilot (ascent only);
  docking (the docking-port part and soft-capture physics exist, there is no docking UI and no
  shipped flow exercises it); the web demo (60-part cap, no craft files or saves).
- **Implemented but off by default:** life support, enabled per new game.
- **Missing:** planes and rovers (the part schema has no wing or wheel modules, and the catalog is a
  rocket stack); stations beyond the docking-port and lab parts; N-body, CommNet, EVA, multiplayer
  and the LLM assistant, all post-v1 by plan.

Work the repository records as deferred or cut:

- A staging design assistant and `MissionProfile` emission; action groups and `mission.ascend` on
  the frozen `ControlApi`; attaching the HTTP interface to a running windowed game; thrust inside
  another body's sphere of influence, which leaves moon missions as ballistic flybys
  (`docs/quarterly/2026Q4-deferrals.md`).
- The advanced render tier, the third body Duva beyond the reduced home-and-moon scope, and the
  docking UI (the cut list in `PLAN.md` §6.4).
- CI rows the nightly workflow records as not implemented yet: the `RapierBackend` physics oracle,
  baseline screenshot comparison, the `csp --smoke` end-to-end test and `xtask lint-quality`
  (`.github/workflows/nightly.yml`).

There is no formal win condition (`docs/GAMEPLAY-GUIDE.md` §1). The determinism promise is replay
on one platform with one toolchain (tier T1 in `PLAN.md` §2.7); identical results across platforms
or versions are not promised.

**CI state.** CI is defined in four workflows under `.github/workflows/`. The per-commit
workflow is set up to run on every push and pull request on GitHub's `ubuntu-latest` runner;
nightly, weekly and dependency-triggered workflows add slower checks. macOS and Windows
runners appear only in a nightly job that reports golden-replay verdicts without failing on
a divergence.

The workflow files say what CI is set up to check, not whether it passed. At the time of
writing (2026-11-24) GitHub Actions is disabled for this repository, and the Actions history
holds two runs in total, both of the per-commit workflow on `main` and both older than the
current workflow files: one on 2026-10-22 that built the workspace, ran the test suite and
passed the wasm check, then failed at the repository-lint step, and one on 2026-10-26 whose
formatting step failed before the run was cancelled. No run has been recorded for any commit
pushed since, and the other three workflows have never run. All four workflow files were
last changed on 2026-10-27, after both runs. Read "verified by CI" and "CI runs" below as "a
workflow in `.github/workflows/` defines that step", and the repository's Actions tab for the
current result.

## Supported platforms

"Verified by CI" means a workflow in `.github/workflows/` defines the step for that platform. Every
other claim is labelled as not verified.

| Platform | Build | Run | Tests | Evidence |
|---|---|---|---|---|
| Linux x86-64 | Verified by CI | Not exercised by CI | Verified by CI | `.github/workflows/ci.yml`, jobs `check` and `perf`; `PLAN.md` §2.7 |
| Linux on other CPU architectures | Expected to work, not verified by this project | Expected to work, not verified by this project | Expected to work, not verified by this project | `data/test-fixtures/*/fingerprint.json` |
| Windows | Expected to work, not verified by this project | Expected to work, not verified by this project | Expected to work, not verified by this project | `.github/workflows/nightly.yml`, job `cross-platform` |
| macOS | Expected to work, not verified by this project | Expected to work, not verified by this project | Expected to work, not verified by this project | `.github/workflows/nightly.yml`, job `cross-platform` |
| Web demo (WebGPU) | Compile check verified by CI; packaging expected to work, not verified by this project | Expected to work, not verified by this project | No browser test | `.github/workflows/ci.yml`, `xtask/src/main.rs` |
| Other platforms | Not supported | Not supported | Not supported | None in the repository |

The reasoning behind each row:

- **Linux x86-64.** Build: verified by CI on every push and pull request: clippy over all
  targets, the test build and the wasm check on `ubuntu-latest`. Run: not exercised by CI, no
  job starts the windowed game; the project's own evidence of running it is the play-session
  findings in `docs/GAMEPLAY-GUIDE.md` §10. Tests: verified by CI: the `check` job of
  `.github/workflows/ci.yml` runs the whole suite with the GPU render tests required, on Mesa's
  lavapipe, the driver that job installs before the tests; no recorded run has reached that step.
- **Linux on other CPU architectures.** CI uses one hosted Linux runner, and the committed
  fingerprints record `x86_64-unknown-linux-gnu` (`data/test-fixtures/*/fingerprint.json`).
- **Windows and macOS.** Build: a nightly job builds `csp-replay` in release mode on
  `windows-latest` and `macos-latest`. That compiles the simulation, UI, view and renderer crates
  as its dependencies, but not the `csp` application crate. Building the game is expected to work,
  not verified by this project. Tests: expected to work, not verified by this project: the nightly
  job replays the goldens and reports the verdict without failing on a divergence.
- **Web demo (WebGPU).** Build: the wasm compile check is verified by CI. The `build-web`
  packaging step is not run by CI: expected to work, not verified by this project. Tests: no
  browser test exists. The demo's embedded data set and fixed scenario are tested natively
  (`crates/csp-view/tests/demo.rs`).
- **Other platforms.** No workflow, setup or documentation exists in the repository for them.

No CI job starts the windowed game on any platform.

The renderer asks wgpu for its primary backends (`Backends::PRIMARY` in
`crates/csp-render/src/lib.rs`), which are Vulkan, Metal, D3D12 and WebGPU together. Which one is
used depends on what the platform provides: Vulkan on Linux, Metal on macOS, D3D12 or Vulkan on
Windows, WebGPU in the browser.

## Repository layout

A Cargo workspace with 13 members: the 12 crates under `crates/` and `xtask` (`Cargo.toml`).
Numbers written as `§N` refer to sections of `PLAN.md`.

| Crate | Role | License |
|---|---|---|
| `csp-core` | Sim clock, vectors and quaternions, state hasher, RNG key derivation, error classes | MIT OR Apache-2.0 |
| `csp-data` | RON schemas, loader and validator, data-set hash | MIT OR Apache-2.0 |
| `csp-phys` | Part weld, effective-part resolver, `SimpleBackend` (contact, aero, thermal), craft session | MIT OR Apache-2.0 |
| `csp-sim` | The authoritative headless simulation; every game verb is a `SimCommand` here | MIT OR Apache-2.0 |
| `csp-script` | Rhai script host and the frozen `ControlApi` | MIT OR Apache-2.0 |
| `csp-ai` | Ascent autopilot (Rust, drives `ControlApi`) | MIT OR Apache-2.0 |
| `csp-view` | `bevy_ecs` presentation world; the only bridge between simulation and renderer | MIT OR Apache-2.0 |
| `csp-render` | wgpu renderer (Baseline tier) and WGSL shaders | MIT OR Apache-2.0 |
| `csp-ui` | Pure egui panels (VAB, HUD, program window, settings); no simulation or wgpu dependency | MIT OR Apache-2.0 |
| `csp-app` | winit shell, frame loop, warp controller; builds the `csp` binary and the wasm library | `LICENSE-GAME` (spelled `LicenseRef-CSP-GAME` in the manifest) |
| `csp-server` | Native-only HTTP, WebSocket and MCP server (axum) | MIT OR Apache-2.0 |
| `csp-bench` | Native-only harness: `csp-replay`, `csp-perf`, `csp-eval`, `csp-mcp` | MIT OR Apache-2.0 |
| `xtask` | Repository lints and tools: `lint-deps`, `lint-attribution`, `lint-reach`, `bless-goldens`, `build-web`, and the `bake_art`, `gen_biome_maps` and `ppm2png` binaries | MIT OR Apache-2.0 |

| Path | Contents |
|---|---|
| `data/` | Game content as RON files (parts, systems, tech tree, contracts, balance), baked art packs under `data/art/`, golden test fixtures under `data/test-fixtures/`, evaluation scenarios under `data/eval-scenarios/` |
| `apps/demo/` | The web demo page and its reduced embedded data set |
| `apps/telemetry/` | The telemetry web app (TanStack Start on Vite) |
| `assets/` | Bake inputs for the art packs; provenance of the source textures in `assets/ASSETS.md` |
| `docs/` | Deep documentation; `docs/perf/` is a measurement log, not documentation |
| `xtask/` | The `xtask` crate |
| `ci/` | Budgets read by CI: the rustdoc warning ceiling and the coverage floors |
| `.github/workflows/` | CI definitions |
| `PLAN.md` | The design and implementation plan; normative, cited as `§N` throughout the code |

## Prerequisites and setup

Building needs the pinned Rust toolchain and a native C toolchain. According to the manifests and
build scripts of the locked dependency versions, nothing else is linked at build time:

- `blake3` (pinned to `=1.8.3` in `Cargo.toml`) compiles its assembly sources through the `cc`
  crate, which is where a compiler, assembler and linker are needed.
- The Vulkan loader is opened at run time: `ash` is used with its default `loaded` feature.
- The X11, XCB and xkbcommon libraries are opened at run time by `x11-dl`, `xkbcommon-dl` and
  `x11rb` (with its `dl-libxcb` feature).
- `libwayland-client` is opened at run time as well: `wayland-sys` is built with its `dlopen`
  feature, which wgpu-hal enables on Unix targets.
- `Cargo.lock` contains no audio library and no TLS library. Sound is played by starting a player
  program (see the operating-system sections), and `csp --serve` has no TLS in v1.

CI cannot show that a bare system needs nothing more, because GitHub's runner image already carries
a C toolchain and system libraries.

### Rust toolchain

The required version is determined by `rust-toolchain.toml` at the repository root:

```toml
[toolchain]
channel = "1.97.1"
components = ["clippy", "rustfmt"]
targets = ["wasm32-unknown-unknown"]
```

Install [rustup](https://rustup.rs) from its official instructions (external to this repository)
and let it pick the channel: rustup installs the pinned 1.97.1 toolchain, with `clippy`, `rustfmt`
and the `wasm32-unknown-unknown` target, the first time a `cargo` command runs inside the
repository. CI relies on the same behavior and installs no toolchain of its own
(`.github/workflows/ci.yml`).

The declared minimum supported Rust version is 1.95 (`rust-version` in `Cargo.toml`). The weekly
workflow only checks that the workspace still compiles on 1.95.0; build with the pinned channel.

### Linux

**Rust toolchain.** The pinned 1.97.1 channel applies unchanged; see
[Rust toolchain](#rust-toolchain).

**System packages.** The only Linux packages this project can vouch for are the two that
`.github/workflows/ci.yml` installs on GitHub's Ubuntu runner before running the tests:
`mesa-vulkan-drivers` and `libvulkan1`. Every other name below is standard practice for the
dependencies, not verified by this project.

#### Fedora

Standard practice for the dependencies, not verified by this project; the names were checked
against Fedora's package database on 2026-11-23. Fedora is not exercised by CI.

```bash
# Build: C compiler, assembler and linker
sudo dnf install gcc
# Run: the Vulkan loader, Mesa's Vulkan drivers (the software lavapipe, plus Intel and AMD)
# and the windowing libraries
sudo dnf install vulkan-loader mesa-vulkan-drivers libX11 libX11-xcb libxcb libXcursor libXi libxkbcommon libxkbcommon-x11 libwayland-client
```

```bash
# Optional: sound players and a Vulkan diagnostic tool
sudo dnf install pipewire-utils pulseaudio-utils alsa-utils vulkan-tools
```

#### Ubuntu and Debian

The two Vulkan packages are the pair that `.github/workflows/ci.yml` installs on `ubuntu-latest`
before the tests, which makes them the only Ubuntu names the repository itself specifies; no
recorded run has reached that step. The remaining names are standard practice for the
dependencies, not verified by this project; they were checked against the Debian trixie package
database on 2026-11-23 (and `pipewire-bin` and `pulseaudio-utils` also against Ubuntu 24.04).

```bash
sudo apt-get update
# Build: C compiler, assembler and linker
sudo apt-get install -y build-essential
# Run: the Vulkan loader, Mesa's Vulkan drivers (the software lavapipe, plus Intel and AMD)
# and the windowing libraries
sudo apt-get install -y mesa-vulkan-drivers libvulkan1 libx11-6 libx11-xcb1 libxcb1 libxcursor1 libxi6 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0
```

```bash
# Optional: sound players and a Vulkan diagnostic tool
sudo apt-get install -y pipewire-bin pulseaudio-utils alsa-utils vulkan-tools
```

#### Arch Linux

Standard practice for the dependencies, not verified by this project; the names were checked
against the Arch Linux package database on 2026-11-23. Arch is not exercised by CI.

```bash
# Build: C compiler, assembler and linker
sudo pacman -S --needed gcc
# Run: the Vulkan loader, the windowing libraries and a Vulkan driver.
# vulkan-swrast is the software lavapipe; add vulkan-intel or vulkan-radeon for a GPU
sudo pacman -S --needed vulkan-icd-loader vulkan-swrast libx11 libxcb libxcursor libxi libxkbcommon libxkbcommon-x11 wayland
```

```bash
# Optional: sound players and a Vulkan diagnostic tool
sudo pacman -S --needed pipewire-audio libpulse alsa-utils vulkan-tools
```

**GPU driver** (standard practice, unverified by this project, apart from lavapipe, which the CI
workflow installs for its tests; no recorded run has reached that step). The packages above let the
game start on a machine without a GPU: lavapipe renders on the CPU and is slow. On Fedora, Ubuntu
and Debian the Mesa package carries the Intel and AMD drivers too; on Arch they are the separate
`vulkan-intel` and `vulkan-radeon` packages. For an NVIDIA GPU, install the vendor's Vulkan driver;
this repository does not document one.

**Headless tests.** The GPU render tests render offscreen and need a Vulkan driver but no display
server. The loader plus lavapipe is the pair the CI workflow installs on Ubuntu before running
`cargo test --workspace` with `CSP_RENDER_GATE_REQUIRED=1`. On Fedora the pair is `vulkan-loader`
and `mesa-vulkan-drivers`, on Arch `vulkan-icd-loader` and `vulkan-swrast`. Without a driver the
tests print `SKIP` and pass.

**Sound.** The build needs no audio package. At run time the game starts the first of `pw-play`,
`paplay` or `aplay` that it can start and that stays connected to its sound server; if none works,
the game keeps running without sound and logs the warning
`no audio player found … — running silent` (`crates/csp-app/src/audio.rs`). The player packages
are therefore optional: `pw-play` comes with `pipewire-utils` (Fedora), `pipewire-bin` (Ubuntu,
Debian) or `pipewire-audio` (Arch), `paplay` with `pulseaudio-utils` (Fedora, Ubuntu, Debian) or
`libpulse` (Arch), and `aplay` with `alsa-utils`.

### Windows

Building and running the game on Windows is expected to work, not verified by this project. The
only Windows evidence in the repository is the nightly job that builds `csp-replay`, described in
[Supported platforms](#supported-platforms).

**Rust toolchain.** The same `rust-toolchain.toml` applies. Rust's default host toolchain on
Windows targets MSVC, which needs the Visual Studio C++ build tools (the "Desktop development with
C++" workload) and the Windows SDK. This is standard practice from the Rust installation
documentation; the nightly workflow installs nothing and relies on what the hosted runner image
provides.

**System packages.** None are documented for Windows. Rendering uses D3D12 or Vulkan through the
graphics driver installed with the GPU (standard practice, unverified by this project).

**Sound.** The game runs silently unless `pw-play`, `paplay` or `aplay` is on `PATH`, and none of
them is a normal part of a Windows installation (derived from `crates/csp-app/src/audio.rs`; not
verified by running).

**Headless tests.** No package list is verified or documented by this project for Windows. The GPU
render tests print `SKIP` and pass when wgpu finds no adapter; with `CSP_RENDER_GATE_REQUIRED=1`
set, that becomes a failure. A software Vulkan driver such as Mesa's lavapipe build for Windows is
the usual answer on a machine without a GPU (standard practice, unverified by this project).

### macOS

Building and running the game on macOS is expected to work, not verified by this project. The only
macOS evidence in the repository is the nightly job that builds `csp-replay`, described in
[Supported platforms](#supported-platforms).

**Rust toolchain.** The same `rust-toolchain.toml` applies. Xcode Command Line Tools
(`xcode-select --install`) provide the compiler and linker. This is standard practice; the nightly
workflow installs nothing and relies on what the hosted runner image provides.

**System packages.** None are documented for macOS. Rendering uses the Metal framework that ships
with macOS.

**Sound.** The game runs silently unless `pw-play`, `paplay` or `aplay` is on `PATH` (derived from
`crates/csp-app/src/audio.rs`; not verified by running).

**Headless tests.** No package list is verified or documented by this project for macOS. wgpu's
primary backends include Metal, so the render tests may find the hardware adapter; this project has
not run them on macOS.

On Windows and macOS, note that the committed golden replays were recorded on
`x86_64-unknown-linux-gnu`. Same-platform replay is the only determinism tier the project
promises, so a different hash there is a reportable finding, not a verified pass (`PLAN.md` §2.7).

## Build and run

All commands are run from the repository root.

### Debug and release builds

```bash
cargo run -p csp-app --bin csp
```

Builds the game in debug mode and starts it.

```bash
cargo run --release -p csp-app --bin csp
```

Builds in release mode and starts it. The performance budgets in `PLAN.md` §5.3 are release
numbers; do not compare debug-build timings with them.

To build without starting, use `cargo build` with the same arguments. The binary is written to
`target/debug/csp` or `target/release/csp` (`csp.exe` on Windows).

### Run from the repository root

Start the game from the repository root, for two reasons:

- Startup uses a directory named `data` in the working directory when it contains `balance.ron`
  and otherwise falls back to the repository's `data/` when that contains `balance.ron`
  (`csp_view::data_root()` in `crates/csp-view/src/lib.rs`). A `data/` directory with a
  `balance.ron` in it that happens to sit in another working directory is therefore used instead
  of the real one.
- Saves and craft files are written to `saves/` and `crafts/` in the working directory, and
  `--serve` uses the same `saves/` (`crates/csp-app/src/lib.rs`).

A missing or invalid data tree aborts startup with `data/ startup load failed`.

### Headless server mode

```bash
cargo run -p csp-app --bin csp -- --serve
```

Runs the simulation without a window and serves the HTTP, WebSocket and MCP interface on the
loopback address, port 47900 (`crates/csp-server/src/serve.rs`). `--port <number>` (or
`--port=<number>`) overrides the port. `--bind-lan` listens on all interfaces, with the bearer token
as the only access boundary because there is no TLS in v1. Any other argument is refused
(`parse_serve_flags` in `crates/csp-app/src/lib.rs`). The server hosts its own fresh world seeded
with the starter craft; it does not attach to a running windowed game. Endpoints and the bearer
token are specified in `docs/BUILD-AND-DEV.md` §5 and `crates/csp-server/PROTOCOL.md`.

### Bug-report mode

```bash
cargo run -p csp-app --bin csp -- --bug-report
```

Writes a diagnostic bundle for a fresh session (save, command log, data-set hash, fingerprint and
the last 500 log lines) under the `csp/bug-reports` folder of the platform configuration directory
and prints the path (`crates/csp-app/src/bugreport.rs`).

Settings live in the same configuration directory as `csp/settings.ron`: `$XDG_CONFIG_HOME/csp`
or `~/.config/csp` on Linux, `%APPDATA%\csp` on Windows, `~/Library/Application Support/csp` on
macOS (`crates/csp-app/src/settings_io.rs`).

## Tests and quality gates

These are the commands listed under "Definition of done" in `AGENTS.md`, which mirror
`.github/workflows/ci.yml`. Shell syntax is shown for a POSIX shell, as in CI; on Windows
PowerShell, set an environment variable with `$env:NAME = "value"` instead.

**Formatting**

```bash
cargo fmt --all -- --check
```

Checks that every file is formatted. CI runs this first.

**Lints**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Builds every target with clippy and treats any warning as an error. `unwrap` and `expect` are
denied workspace-wide.

**Tests**

```bash
cargo test --workspace
```

Runs every unit and integration test. The GPU render tests render offscreen; without a GPU driver
they print `SKIP` and pass. CI runs the suite with the skip turned into a failure:

```bash
CSP_RENDER_GATE_REQUIRED=1 cargo test --workspace
```

**WebAssembly compile check**

```bash
cargo check --workspace --exclude csp-server --exclude csp-bench --exclude xtask --target wasm32-unknown-unknown
```

Checks that the game still compiles for the web target. The three excluded crates are native-only.

**Repository lints**

```bash
cargo run -p xtask -- lint-deps
```

Checks each crate's dependency list against the per-crate allow and deny lists.

```bash
cargo run -p xtask -- lint-attribution
```

Checks that every non-authored file under `data/` and `assets/` has an attribution line with an
allowlisted license.

```bash
cargo run -p xtask -- lint-reach
```

Checks that every `SimCommand` has a production issuer in the UI, view or app crate.

CI runs these as three separate commands, and there is no `cargo xtask` alias: always use
`cargo run -p xtask --`.

**Dependency licenses and bans**

```bash
cargo deny check
```

Checks the dependency tree against `deny.toml`: a closed license allowlist and workspace-wide
bans. This needs `cargo-deny`. The CI workflow installs it with the `taiki-e/install-action` GitHub
Action; locally, install it by any method, for example `cargo install cargo-deny` or
`cargo binstall cargo-deny` (standard practice, not a command this repository defines).

**Golden replays**

```bash
cargo build -p csp-bench --bin csp-replay
for s in $(./target/debug/csp-replay list); do
  ./target/debug/csp-replay verify "data/test-fixtures/$s" || exit 1
done
```

Replays every registered scenario (`trivial` and `flight-launch` are registered at the time of
writing) and compares the reconstructed state hash with the committed one. This is CI's loop,
written in bash. On another shell, list the scenarios and verify each one in the form given by
`AGENTS.md`:

```bash
cargo run -p csp-bench --bin csp-replay -- list
```

```bash
cargo run -p csp-bench --bin csp-replay -- verify data/test-fixtures/flight-launch
```

**Performance budgets** (for performance-sensitive changes)

```bash
cargo run --release -p csp-bench --bin csp-perf -- --out target/perf-log.ron --baseline docs/perf/perf-log-devbox-release.ron
```

Measures the release build against the committed log and fails on a regression of more than 10
percent, a budget crossing from pass to fail, or an unmeasured row. Without `--out`, the log is
written to `docs/perf/perf-log-<machine>-<profile>.ron`, where `<machine>` is `ci-host` under CI
and the hostname otherwise; when that resolves to the baseline file, `csp-perf` refuses to run
rather than overwrite it (`crates/csp-bench/src/bin/csp-perf.rs`, `crates/csp-bench/src/perf.rs`).
The committed baseline is one machine's measurement, so on other hardware the comparison is
cross-machine; `csp-perf` prints a note when the machine names differ. One perf row renders through
a headless adapter, so this gate needs a Vulkan driver as well.

**Generated artifacts** (also checked by the per-commit workflow on every push and pull request)

```bash
cargo run -p xtask --bin bake_art -- --check
```

Checks that the committed art packs are byte-identical to a fresh bake.

```bash
cargo run -p xtask --bin gen_biome_maps -- --check
```

Checks that the committed biome maps are byte-identical to the generator's output.

CI additionally gates `cargo doc` against the rustdoc warning ceiling in
`ci/doc-warning-budget.txt` and coverage against the floors in `ci/coverage-floors.json`. The
workflow tiers:

| Workflow | Trigger | What it runs |
|---|---|---|
| `.github/workflows/ci.yml` | Every push and pull request, on `ubuntu-latest` | Format, clippy, tests, wasm check, repository lints, generated-artifact checks, cargo-deny, golden replays; a release-profile perf gate; the telemetry app's typecheck, tests and build on Node 24; the rustdoc warning ceiling; the coverage floors |
| `.github/workflows/nightly.yml` | Nightly and on demand | Golden replays in release, the economy harness, the eval suite, cargo-udeps and the perf baseline refresh on `ubuntu-latest`; the report-only `cross-platform` replay job on `macos-latest` and `windows-latest` |
| `.github/workflows/weekly.yml` | Weekly and on demand | cargo-mutants on the four determinism crates, and an MSRV check with Rust 1.95.0 |
| `.github/workflows/deps.yml` | Pull requests that change dependency manifests, weekly, on demand | cargo-machete (unused dependencies) |

## Web demo and telemetry web app

### Web demo

The web build is a fixed demo: one planet, a cap of 60 parts and no saves (`PLAN.md` §6.3).
Packaging needs `wasm-bindgen-cli` at the version pinned in `Cargo.toml`, 0.2.129:

```bash
cargo install wasm-bindgen-cli --version 0.2.129 --locked
```

Builds and installs the matching `wasm-bindgen` command line tool.

```bash
cargo run -p xtask -- build-web
```

Builds the game library for `wasm32-unknown-unknown` in release mode, runs `wasm-bindgen` and
writes `apps/demo/dist`, which is ignored by git.

```bash
python3 -m http.server -d apps/demo/dist
```

Serves the output. This is the example command that `build-web` prints; any static file server
works.

Viewing the demo needs a browser with WebGPU: Chrome or Edge 113 or later, Firefox 141 or later,
Safari 26 or later (`apps/demo/index.html`). The per-commit workflow is set up to compile the wasm
target on every push and pull request, and no step runs the packaged demo in a browser.

### Telemetry web app

`apps/telemetry` is a single-page app that shows live telemetry from a running `csp --serve`. CI
runs its typecheck, tests and build on Node 24; `apps/telemetry/package.json` declares no Node
version of its own.

```bash
cd apps/telemetry
npm ci
npm run build
```

Installs the locked dependencies and builds the static bundle to `apps/telemetry/dist/client`,
which `csp --serve` then serves at `/`. Without that build, the server answers with its built-in
fallback page.

For development, `npm run dev` starts the Vite dev server on port 47901 and proxies `/ws` and
`/api` to the game server; `npm test` runs the test suite and `npm run typecheck` type-checks the
sources. The dev server needs the bearer token that `csp --serve` writes to
`$XDG_RUNTIME_DIR/csp/<pid>.token` (first line): pass it once as the `csp_token` query parameter,
or export it as `VITE_CSP_TOKEN`. Details are in `apps/telemetry/README.md`.

## Content and art tooling

Two tools in `xtask` generate committed content, and a third converts render test dumps:

```bash
cargo run -p xtask --bin bake_art
```

Bakes the procedurally authored part meshes and the terrain textures in `assets/src/` into the
`.csar` packs under `data/art/`. Rerun it when either input changes, then commit the packs.

```bash
cargo run -p xtask --bin gen_biome_maps
```

Regenerates the biome maps under `data/systems/home/` from the authored region rules. Rerun it
after editing a rule or a `biomes.ron` legend, then commit the PNGs.

```bash
cargo run -p xtask --bin ppm2png -- /tmp/frame.ppm
```

Converts a PPM frame written by the render dump tests to PNG, next to the input by default.

## Troubleshooting

Rows are taken from `docs/BUILD-AND-DEV.md` §10, which has the full table.

| Symptom | Cause and fix |
|---|---|
| `data/ startup load failed` at launch | Not started from the repository root, or a broken `data/` tree. Start `cargo run -p csp-app --bin csp` from the repository root. |
| Render-gate tests print `SKIP` | No Vulkan driver was found. Install the loader and lavapipe (`mesa-vulkan-drivers libvulkan1` on Ubuntu) or accept the skip; `CSP_RENDER_GATE_REQUIRED=1` turns the skip into a failure, as in CI. |
| The wasm build fails on `axum`, `tokio`, `csp-server` or `csp-bench` | The whole workspace was built for wasm. Use the exclusion set shown in [Tests and quality gates](#tests-and-quality-gates). |
| `build-web` reports `wasm-bindgen not found` | Run `cargo install wasm-bindgen-cli --version 0.2.129 --locked`; the CLI version must match the crate pin. |
| A golden replay goes red after a dependency bump | Expected when a dependency in the determinism domain changed. Check `fingerprint.json` in the golden directory first. For a legitimate change, bump `HASH_SCHEMA_VERSION` or pass `--reason` with a text, then run `cargo run -p xtask -- bless-goldens` on a clean tree. Never edit `expected-hash.txt` by hand. |
| Performance is far below the budgets | A debug build. Budgets are release numbers; run `csp-perf` in release with `--baseline docs/perf/perf-log-devbox-release.ron`. |
| The browser shows the server's fallback page instead of telemetry | `apps/telemetry/dist/client` has not been built. Build it as shown above, then restart `csp --serve`. |
| `cargo xtask …` fails | There is no `cargo xtask` alias. Use `cargo run -p xtask --` followed by the task name. |

## Documentation index and licensing

### Documentation index

| Document | Subject |
|---|---|
| [`PLAN.md`](PLAN.md) | The design and implementation plan: decisions, architecture, game design, milestones, testing gates. Normative. |
| [`AGENTS.md`](AGENTS.md) | Workspace map, verified commands, hard rules and the definition of done, written for contributors and coding agents |
| [`docs/BUILD-AND-DEV.md`](docs/BUILD-AND-DEV.md) | The full command map, the test map, the CI description and the troubleshooting table |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | The crate graph, frame and data flow, determinism architecture, threading and known differences between `PLAN.md` and the code |
| [`docs/SIMULATION.md`](docs/SIMULATION.md) | The simulation core: tick lifecycle, commands, events, physics, orbits, progression, save and replay |
| [`docs/UI-AND-RENDERING.md`](docs/UI-AND-RENDERING.md) | The presentation stack: view world, egui panels, the VAB, flight and map views, the renderer |
| [`docs/DATA-AND-SCRIPTING.md`](docs/DATA-AND-SCRIPTING.md) | The `data/` schemas, the Rhai host, autopilots, server and MCP automation, telemetry screens |
| [`docs/GAMEPLAY-GUIDE.md`](docs/GAMEPLAY-GUIDE.md) | What is playable: controls, the VAB, flight, the science loop, the program window, warp, saves |
| [`docs/quarterly/2026Q4-deferrals.md`](docs/quarterly/2026Q4-deferrals.md) | The cut and deferral decisions recorded by the quarterly review |
| [`docs/perf/perf-log-devbox-release.ron`](docs/perf/perf-log-devbox-release.ron) | A committed performance measurement log used as a regression baseline; a log, not documentation |
| [`crates/csp-server/PROTOCOL.md`](crates/csp-server/PROTOCOL.md) | Protocol pins for the server's MCP and HTTP surface |
| [`apps/telemetry/README.md`](apps/telemetry/README.md) | Build, development flow and structure of the telemetry web app |

### Licensing and attribution

The license is set per crate, never workspace-wide (`Cargo.toml`).

- **Engine crates.** The crates under `crates/` whose manifest declares `license = "MIT OR Apache-2.0"`,
  and `xtask`, are dual-licensed under the MIT and Apache-2.0 licenses; see
  [`LICENSE-MIT`](LICENSE-MIT) and [`LICENSE-APACHE`](LICENSE-APACHE) (which refers to the canonical
  Apache-2.0 text rather than reproducing it).
- **The game.** The `csp-app` crate (the shipped `csp` binary target and the wasm library built
  from the same crate), `data/` and `apps/` are proprietary and source-available under
  [`LICENSE-GAME`](LICENSE-GAME) (the `csp-app` manifest spells the license `LicenseRef-CSP-GAME`).
  In its wording: you may view, modify and build the source for personal, non-commercial use; you
  may not redistribute the source, binaries or assets, publicly host modified versions, or use any
  part of these components in a commercial product or service without written permission.
  [`data/LICENSE`](data/LICENSE) repeats this for the data directory.

**Asset attribution.** Every file under `data/` or `assets/` that was not authored in the
repository needs one line in `data/ATTRIBUTION.md` with four pipe-separated fields: path, source,
license and URL. The license must be on the allowlist at the top of that file (CC0, public domain,
MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, Zlib, BSL-1.0, OFL-1.1, Unlicense, CC-BY-3.0 and
CC-BY-4.0). `AGENTS.md` states the working rule more narrowly: non-commercial and share-alike
assets are unusable, and CC0 or MIT only. `cargo run -p xtask -- lint-attribution` fails when a
file has no line, or a line names a license outside the allowlist. `assets/ASSETS.md` records the
provenance of the texture sources.

**Dependency licenses.** `deny.toml` holds a closed allowlist of dependency licenses and
workspace-wide bans on the `rayon` and `bevy` crates; `cargo deny check` enforces both in CI.

### Conventions

- **Commits** on `main` follow `type(scope): summary`, for example a subject starting with
  `fix(sim):` or `feat(game):`. Regenerated goldens are their own `chore(goldens):` commit that
  names the reason.
- **Content is data.** Parts, systems, tech, contracts and balance are RON files under `data/`,
  loaded and validated by `csp-data`. Adding content means editing data and its validators, never
  adding a code path to the simulation.
- **Simulation invariants**, enforced by lints: no wall clock, no hash maps, no `rand` facade, no
  `f64` transcendentals outside `libm`, no `unwrap` or `expect` anywhere in the workspace, and
  state-changing iteration in ascending ID order.
