# Clanker Space Program

Clanker Space Program is a KSP-style build-and-fly space sim with an RTS-style program layer, set in
a fictional solar system and written in Rust. You assemble a rocket in the editor, pay for the
launch, and fly it. Recovering it brings back science and funds for the next one.

Between flights you run the program: research, crew, colonies, timewarp. The parts, the tech, the
contracts and the balance are all RON files under `data/`, not code.

## What you need

You need Rust from [rustup](https://rustup.rs), a C compiler and, on Linux, a Vulkan driver.
The repository pins the Rust version in `rust-toolchain.toml`. rustup installs that version
on the first `cargo` run.

On Linux, the game pipes sound into `pw-play`, then `paplay`, then `aplay`. No player, no sound. The
render tests need a Vulkan driver to draw offscreen and skip without one; on a machine with no GPU,
Mesa's software driver is the one to install.

It's developed and tested on Fedora; the Windows and macOS builds are untested.

## Linux

Same pieces everywhere: a C compiler, the Vulkan loader and a driver, the X11 and Wayland libraries,
and PipeWire's player. Only the package names change.

### Fedora

```bash
sudo dnf install gcc vulkan-loader mesa-vulkan-drivers libX11 libX11-xcb libxcb libXcursor libXi libxkbcommon libxkbcommon-x11 libwayland-client pipewire-utils
```

### Ubuntu and Debian

`build-essential` is the compiler.

```bash
sudo apt install build-essential libvulkan1 mesa-vulkan-drivers libx11-6 libx11-xcb1 libxcb1 libxcursor1 libxi6 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 pipewire-bin pulseaudio-utils
```

### Arch Linux

On an Intel or NVIDIA GPU, swap `vulkan-radeon` for `vulkan-intel` or `nvidia-utils`.

```bash
sudo pacman -S --needed base-devel vulkan-icd-loader vulkan-radeon libx11 libxcb libxcursor libxi libxkbcommon libxkbcommon-x11 wayland pipewire-audio
```

## Windows

Install the [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/) with the
"Desktop development with C++" workload, and an up-to-date driver from your GPU vendor.

## macOS

Xcode's command line tools bring the compiler.

```bash
xcode-select --install
```

## Build and run

```bash
cargo run -p csp-app --bin csp                  # run the game (from repo root)
cargo run --release -p csp-app --bin csp        # release build
```

The game looks for `data/` in the working directory first, so run both from the repository root.
`--serve` starts the game headless, with HTTP, WebSocket and MCP on 127.0.0.1:47900. `--bug-report`
writes a diagnostic bundle with the save, the command log and the last 500 log lines.

## Tests

```bash
cargo test --workspace
cargo run -p xtask -- lint-deps
cargo run -p xtask -- lint-attribution
cargo run -p xtask -- lint-reach
```

[AGENTS.md](AGENTS.md) lists the full set of gates to clear before a change counts as done.

## Where things are

The design is in [PLAN.md](PLAN.md).

Open [BUILD-AND-DEV.md](docs/BUILD-AND-DEV.md) for every command the repository has,
[GAMEPLAY-GUIDE.md](docs/GAMEPLAY-GUIDE.md) for how the game is played and
[ARCHITECTURE.md](docs/ARCHITECTURE.md) for how the code fits together.

## License

The engine crates are MIT OR Apache-2.0; `data/`, `apps/` and the shipped binary are proprietary
([LICENSE-GAME](LICENSE-GAME)).
