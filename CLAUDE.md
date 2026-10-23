# Catlock

Keyboard lock for cat people on Linux (Rust, GTK 4 + libadwaita). See `README.md` for features and
user-facing behavior.

## Commands

```sh
cargo build                # debug build; icons load from data/icons
cargo run                  # run from the source tree
make test                  # cargo test (detector, layout, config, sound)
cargo fmt                  # rustfmt.toml: max_width 120, use_small_heuristics Max
cargo clippy --all-targets
make install               # release build into ~/.local (no root)
```

Only one instance runs per session (D-Bus name `io.github.catlock.Catlock`); stop a running one with
`catlock --quit` before testing a new build.

## Architecture

| Module | Role |
|---|---|
| `src/engine.rs` | tokio task that grabs every keyboard via evdev and re-emits through one uinput device; locking = stop forwarding. Matches the unlock shortcut, runs Smart Detect, talks to the UI over channels (`Command` in, events out) |
| `src/uinput.rs` | Virtual keyboard on raw ioctls (the evdev crate cannot declare LEDs, needed for Caps/Num Lock) |
| `src/detect.rs` | Smart Detect rules (paw, resting, hold, zoomies) and sensitivity thresholds; most unit tests live here |
| `src/keys.rs` | Physical key geometry (ANSI grid) shared by the detector and the heat map |
| `src/config.rs` | TOML settings in `~/.config/catlock/config.toml`, profiles |
| `src/sound.rs` | Built-in clips (`include_bytes!` from `assets/sounds`), imported clips, playback thread (rodio) |
| `src/system.rs` | Screen-lock / sleep signals (auto-unlock), launch at login |
| `src/tray.rs` | StatusNotifierItem tray via ksni |
| `src/ui/` | GTK controller (`mod.rs`), settings window (pages rebuilt, never patched), per-monitor lock overlay, heat map |
| `src/main.rs` | CLI flags; query-only flags (`--status`, `--unlock`, `--quit`) must never start an instance |

GTK owns the main thread; the engine and tray run on a 2-worker tokio runtime.

## Conventions

- Safety invariant: a crash or exit must always free the keyboards (kernel drops evdev grabs). Never
  add state that keeps input blocked outside the running process.
- Held keys must be released on lock so nothing auto-repeats in the focused app.
- Release builds must not embed source-tree paths (`dev_icon_dir` is `debug_assertions` only).
- Sounds are regenerated, not hand-edited: `uv run --no-project python tools/gen_sounds.py`
  (needs `espeak-ng`).
- Headless screenshots of every page: see the `CATLOCK_SNAPSHOT` recipe in `README.md`.

## Packaging (Arch)

`packaging/arch/PKGBUILD` builds from a local tarball of the committed tree, so commit first:

```sh
make dist                                   # git archive HEAD -> packaging/arch/catlock-<ver>.tar.gz
cd packaging/arch && makepkg -si --cleanbuild
```

- Bump `pkgrel` in its own `build(arch)` commit after user-visible fixes; bump `version` in
  `Cargo.toml` (and reset `pkgrel=1`) for releases.
- Runtime deps go in `depends`; the post-install hook reminds users to join the `input` group.
