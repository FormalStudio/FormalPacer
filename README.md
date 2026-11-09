<p align="center"><img src="packaging/linux/dev.formalsnake.formalpacer.png" width="160" alt="FormalPacer icon"></p>

# FormalPacer

Console-style frame pacing for Linux and Windows games. FormalPacer watches the game you are
playing, works out the frame rate it can hold on the display it is on, and caps it there, so
frames arrive at an even rhythm instead of whenever they are ready. It lives in the tray, with a
small window and an in-game overlay. Rust on
[NativeDesktop](https://github.com/FormalStudio/NativeDesktop).

<p align="center"><img src="docs/images/main.png" width="560" alt="The FormalPacer window holding Elden Ring at 48 fps, a third of 144 Hz"></p>

## How it picks the cap

A Vulkan layer in the game (or, for OpenGL, a preloaded hook) sleeps before each present until
the cap's next deadline and reports how long the game needed for the frame. The app finds the
display the game is on, reads its refresh rate and VRR state, and plans from what the game can
sustain with 8% in reserve:

| Display | Cap | 144 Hz panel, game does 200 / 100 / 60 fps |
|---|---|---|
| Fixed refresh | The largest whole fraction of the refresh rate the game can hold, so every frame stays on screen for the same number of refreshes | 144 / 72 / 48 |
| VRR on | Just under the refresh rate when the game is fast, otherwise the highest rate it holds steadily. Never under the bottom of the VRR window | 138 / 85 / 48 |

The cap drops as soon as the game can no longer hold it and rises only after the game has held
the higher rate for five seconds. Loading screens and alt-tab gaps don't count. The full rules
are in the module docs of `crates/formalpacer-engine/src/pacing.rs`.

The display is the one you picked in the window. Otherwise it is the one the compositor
(Hyprland, Sway, niri) or the X server says the game's window is on, then the one a fullscreen
game's size matches, then the one with VRR running, then the fastest. The window shows which of
these decided it.

## Overlay

<p align="center"><img src="docs/images/overlay.png" width="312" alt="The overlay: 60 FPS, CAP 60, 120HZ, over a flat frame time graph"></p>

The game's top-left corner shows the frame rate, the cap, VRR state and the rate the display
refreshes at, over a graph of recent frame times. An evenly paced game draws a flat line; a
frame that took a quarter longer than the cap's interval or more shows red. Above, a test game rendering
100 fps is held at half of a 120 Hz display without VRR.

Right Shift + F10 toggles it in game (on Linux through the X server, so Proton and other XWayland
games). The tray item and the window toggle it too, and `formalpacer --toggle-overlay` does it
from a compositor key binding, which also covers Wayland-native games. The setting is shared by
every game.

## Install

### Linux

Needs Rust 1.95 (pinned by `rust-toolchain.toml`), the `i686-unknown-linux-gnu` target for 32-bit
games (`rustup target add i686-unknown-linux-gnu`), 32-bit glibc headers (`glibc-devel-32bit` and
`gcc-32bit` on openSUSE, `lib32-glibc` on Arch, `gcc-multilib` on Debian),
[just](https://github.com/casey/just), and `libxkbcommon`, `wayland`, `vulkan-loader`,
`fontconfig` and `freetype` to build against.

```sh
git clone https://github.com/FormalStudio/FormalPacer
cd FormalPacer
just install      # the app, both layers and the OpenGL hooks, a desktop entry and a login item
```

Start FormalPacer once; after that it starts in the tray at login. Games that were already
running need a restart. If the layer is not registered, the window offers to register it.
`just uninstall` removes everything `install` put down.

### Windows

Needs Rust 1.95 and the Visual Studio build tools.

```powershell
git clone https://github.com/FormalStudio/FormalPacer
cd FormalPacer
powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1
```

The script builds the app, the layer and the hook, downloads PresentMon (pinned, hash checked),
installs them in `%LOCALAPPDATA%\Programs\FormalPacer`, registers the layer, and adds
FormalPacer to the Start menu and to sign-in. Once the script has fetched PresentMon,
`nd package windows` builds a `Setup.exe` with the same files.

## Usage

On Linux, Vulkan games are picked up by themselves: anything Steam or Proton starts, Wine and
Lutris games, and any `.exe`, 64-bit and 32-bit. Launchers, gamescope and the Steam client are
left alone. On Windows a game is anything presenting from where the stores install games (Steam,
Epic, GOG, Xbox, Riot, EA, Ubisoft, Battle.net), or whose own window is in the foreground and
covers its monitor. Steam and Epic games show their store name.

Set these in a game's launch options:

| Option | Effect |
|---|---|
| `FORMALPACER=1 %command%` | Pace a game FormalPacer does not recognise |
| `FORMALPACER=0 %command%` | Leave this game alone |
| `DISABLE_FORMALPACER=1 %command%` | Keep the layer out of the process entirely |
| `formalpacer run %command%` | Linux, OpenGL games: start the game with the OpenGL hook preloaded |

Closing the window leaves pacing and the tray item running; launching FormalPacer again brings
the window back. The tray item shows the game and its cap, toggles pacing and the overlay, and
quits. `formalpacer --background` starts with only the tray item, and `formalpacer --headless`
runs the engine with no window and logs what it does to stderr.

## Configuration

The window writes `~/.config/formalpacer/config.toml` (`%APPDATA%\FormalPacer\config.toml` on
Windows):

```toml
mode = "auto"        # "off" measures without capping
display = "DP-1"     # leave out to follow the game (DISPLAY1 on Windows)
overlay = true
```

On Windows FormalPacer also keeps its own bookkeeping there (`[driver_caps]`, `[driver_runs]`,
`[rtss_profiles]`): the limits it set and what each game had before, so it can put them back.

## Windows and anti-cheat

Nothing of FormalPacer's loads into a game under a kernel anti-cheat (Easy Anti-Cheat, BattlEye,
Vanguard, Ricochet, VAC and others). A game is paced one of three ways:

- Vulkan games get the same layer as on Linux, with live pacing and the overlay.
- Direct3D 11, Direct3D 12 and OpenGL games without an anti-cheat get a hook loaded into the
  running game, with live pacing and the overlay, so there is nothing to restart.
- Anti-cheat games, and any game the hook cannot reach, are measured from outside with
  [PresentMon](https://github.com/GameTechDev/PresentMon). With
  [RTSS](https://www.guru3d.com/download/rtss-rivatuner-statistics-server-download/) running,
  the cap goes to its frame rate limiter and applies from the next frame; the overlay is RTSS's
  on-screen display. Without RTSS, the cap goes to the game's NVIDIA driver profile, which
  applies from the game's next launch, and the overlay is a click-through window over the game's
  corner, hidden while VRR is running (any window on top turns G-SYNC off).

FormalPacer only writes RTSS profiles and the driver profile, never the game, and puts the old
limits back when pacing is switched off. Writing RTSS's profiles needs an administrator once.
Measuring from outside needs an administrator or membership of Performance Log Users; the window
offers to add you (one consent prompt, then sign out and back in).

## Limits

- Windows: a game measured from outside on a machine with neither RTSS nor an NVIDIA driver is
  measured only. The hook does not reach 32-bit games or games running as administrator, and
  draws no overlay on 10-bit back buffers (HDR10 among them).
- Linux: a windowed Wayland-native game on KDE or GNOME falls back to the VRR display, since
  neither compositor tells another process where a window is (fullscreen ones are found by
  their size). OpenGL games need `formalpacer run`, since only a Vulkan layer is loaded into
  every game by itself.

## Development

```
crates/formalpacer-shm      the shared page between layer and app
crates/formalpacer-hook     what runs in the game: detection, the limiter, the overlay, its hotkey
crates/formalpacer-layer    the Vulkan layer (cdylib): pacing and the overlay by buffer copy
crates/formalpacer-gl       the OpenGL hook (cdylib, LD_PRELOAD): pacing and the overlay by clears
crates/formalpacer-inject   the Windows hook for Direct3D and OpenGL games
crates/formalpacer-engine   displays, placement, the planner, the engine thread
src/                        the nd app: window, tray item, single instance
tools/fakegame              a Vulkan test game with a set frame rate
```

```sh
just dev                          # run from the checkout; FORMALPACER_DEMO=1 opens on a fixed snapshot
nd check                          # fmt, clippy, framework lints, appearance audit
nd test                           # tiers 1 and 2
cargo test --workspace            # the planner, placement, EDID and the shared page
just pacing-check 200 100 60      # fakegame at each rate under the headless engine
FAKEGAME_FLAGS=--fullscreen just pacing-check
```

## License

MIT
