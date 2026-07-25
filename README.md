# rusty-clicker

A cross-platform autoclicker (macOS, Windows, Linux) with a CLI and a GUI.

## Layout

Cargo workspace, three crates:

| Crate  | What it is |
|--------|-----------|
| `core` | The clicking logic: a `ClickPlan` (button, interval, repeat, target) executed by a runner behind swappable seams — `Clicker` (mouse synthesis via [enigo](https://crates.io/crates/enigo)), `Sleeper`, and an injected RNG. No I/O, unit-tested. |
| `cli`  | Command-line frontend. `clap` flags → `ClickPlan`; Ctrl-C stops. |
| `gui`  | [egui](https://crates.io/crates/egui) desktop app: live click count, rebindable global start/stop hotkey, settings persisted to disk. |

Both frontends are thin adapters over `core`.

## Build

```sh
cargo build --release
```

## CLI

```sh
cargo run -p rusty-clicker-cli -- [OPTIONS]
```

| Flag | Default | Meaning |
|------|---------|---------|
| `--interval <MS>` | `100` | Milliseconds between clicks. |
| `--jitter-max <MS>` | — | Randomize each gap in `[interval, jitter-max]`. |
| `--button <left\|right\|middle>` | `left` | Mouse button. |
| `--times <N>` | — | Stop after N clicks. |
| `--duration <SECS>` | — | Stop after N seconds (mutually exclusive with `--times`). |
| `--at <X,Y>` | current cursor | Click a fixed screen point. |

Omitting `--times`/`--duration` clicks until you press Ctrl-C.

```sh
# 5 left clicks at the cursor, ~10/sec
cargo run -p rusty-clicker-cli -- --times 5

# Right-click a fixed point twice a second with jitter, for 30s
cargo run -p rusty-clicker-cli -- --button right --at 500,400 --interval 400 --jitter-max 600 --duration 30
```

## GUI

```sh
cargo run -p rusty-clicker-gui
```

- Instrument-style readout: configured clicks/sec when idle, live click count while running.
- Timing (interval + optional jitter), button, target (cursor or fixed point), and stop condition (until stopped / a set count / for a time).
- **Global start/stop hotkey** — default `Ctrl+Shift+F6`, rebindable via **Change**; works even when the window isn't focused.
- Settings and the hotkey are saved automatically and restored on next launch.

### Config file

Settings live in the OS-conventional config directory as `config.toml`:

| OS | Path |
|----|------|
| macOS | `~/Library/Application Support/rusty-clicker/config.toml` |
| Linux | `~/.config/rusty-clicker/config.toml` |
| Windows | `%APPDATA%\rusty-clicker\config.toml` |

It's plain TOML — hand-editable if you ever need to fix a binding.

## macOS permissions

macOS gates synthetic input and global hotkeys behind Accessibility. If clicks don't register or the hotkey does nothing, grant the app (or your terminal) access under **System Settings → Privacy & Security → Accessibility**.

## Test

```sh
cargo test -p rusty-clicker-core
```
