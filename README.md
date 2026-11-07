<p align="center"><img src="packaging/linux/dev.formalsnake.formalpacer.png" width="160" alt="FormalPacer icon"></p>

# FormalPacer

Console-style frame pacing for Linux and Windows games. FormalPacer watches the game you are playing, works
out what frame rate it can actually hold on the display it is on, and caps it there, so frames
arrive at an even rhythm instead of whenever they are ready. Rust on
[NativeDesktop](https://github.com/FormalStudio/NativeDesktop) (GPUI on Linux), with a tray item,
a small window and an in-game overlay.

![The window holding a game at a third of 144 Hz](docs/images/main.png)

```
┌──────────────────────────┐  /dev/shm page per game  ┌───────────────────────────────────┐
│ game (Vulkan, DXVK,      │  frame times ──────────▶ │ formalpacer (nd, GPUI)            │
│ VKD3D-Proton, OpenGL)    │                          │ engine thread: displays (DRM),    │
│  └ formalpacer layer     │ ◀── cap, overlay, VRR    │ the game's monitor, the planner   │
│    sleeps to the cap,    │                          │ tray item, window                 │
│    draws the overlay     │                          │                                   │
└──────────────────────────┘                          └───────────────────────────────────┘
```

An implicit Vulkan layer hooks `vkQueuePresentKHR` in every game. Before each present it sleeps
until the cap's next deadline, then records how long the frame took and how much of that was its
own sleep, which tells the app what the game could do uncapped. The app reads each display's mode
and VRR state from the kernel, finds the monitor the game's window is on, and writes the cap
back. On a 144 Hz panel, a game that renders 60 fps is held at 48 with its 1% low at 48, and one
that renders 200 fps is held at 138 with VRR on. OpenGL games get the same through a preloaded
hook on `glXSwapBuffers` and `eglSwapBuffers`.

## How it picks the cap

The planner takes the 95th percentile of the game's own frame time over the last two seconds
(what it can sustain, hitches included), keeps 8% of that in reserve, then:

| Display | Rule | 144 Hz panel, game does 200 / 100 / 60 fps |
|---|---|---|
| Fixed refresh | The largest whole fraction of the refresh rate the game can hold, so every frame stays on screen for the same number of refreshes: the console 30/40/60 rule | 144 / 72 / 48 |
| VRR on | Just under the refresh rate (`r - r²/3600`) when the game is fast, to stay inside the VRR window. Otherwise the highest steady rate in 5 fps steps, or a fraction of the refresh rate when that costs at most 10% | 138 / 85 / 48 |
| VRR on, game near the bottom of the VRR window | Never under the window: there the driver shows frames twice, which many panels show as flicker. A game that just reaches the bottom is held there with less headroom; one that cannot is not capped | |

The cap drops as soon as the game can no longer hold it and only rises after the game has held
the higher rate for five seconds, so it never hunts. A game locked to the refresh rate by vsync
is recognised as such and not mistaken for a slow one. Loading screens and alt-tab gaps are left
out of the numbers.

Some engines (BeamNG, most large ones) build the next frame on other threads while the layer
holds the present, so under a cap they look far faster than they are. When a raise has to be
taken back within ten seconds, FormalPacer stops trusting that game's numbers for raising: it
goes up one step at a time from then on, and waits 30 seconds before trying again after a failed
step, doubling up to five minutes.

## Which display

Pacing is only right for the display the game is on. The app asks, in order:

1. The display you picked in the window, if you picked one.
2. The compositor, for the game's process: Hyprland, Sway and niri.
3. The X server, for the window the game created its swapchain on (XWayland games on KDE and
   GNOME, and plain X11 sessions). The layer reports the window id.
4. The display whose mode matches the game's swapchain size, which is where a fullscreen game
   is. Two displays of the same size are told apart by which one has VRR running.
5. The display with VRR running, which compositors switch on for the fullscreen game.
6. The fastest display.

The window shows which of these decided it. VRR state is the kernel's `VRR_ENABLED` on the
display's CRTC, so "On" means VRR is running right now, not that it is allowed. The VRR range
comes from the monitor's EDID.

## Overlay

![The overlay over a Proton game: 138 FPS, CAP 138, VRR ON](docs/images/overlay.png)

The top-left corner of the game shows the frame rate, the cap and whether VRR is running on the
game's display. It is drawn into the game's own frames, so it shows up in fullscreen, in
screenshots and in recordings. Right Shift + F10 toggles it in game (through the X server, so
Proton and other XWayland games). The tray item and the window toggle it too, and
`formalpacer --toggle-overlay` does it from a compositor key binding, which also covers
Wayland-native games. The setting is shared by every game.

## Windows

On Windows a game is paced one of two ways, and nothing of FormalPacer's goes inside a game that
runs under a kernel anti-cheat.

- Vulkan games get the same layer as on Linux, registered for the user under
  `HKCU\SOFTWARE\Khronos\Vulkan\ImplicitLayers`, with live pacing and the overlay. The layer
  passes every call straight through in a game protected by Easy Anti-Cheat, BattlEye, Vanguard,
  Ricochet or VAC: it finds the anti-cheat's files next to the game (`EasyAntiCheat`, `BattlEye`,
  `*_EAC_EOS.exe`, `*_BE.exe`) or knows the game by name.
- Every other game, Direct3D and OpenGL ones and anti-cheat games like Fortnite, is measured
  from outside through Event Tracing for Windows with Intel's
  [PresentMon](https://github.com/GameTechDev/PresentMon), and its cap goes to the NVIDIA
  driver's frame rate limiter in the game's driver profile (Max Frame Rate in the NVIDIA app).
  Nothing is loaded into the game. The driver reads the profile when a game starts, so a cap
  planned during one run holds from the next launch, and the window says what the driver holds
  for the run now going. Only a game that has been in the foreground gets a driver cap, and
  switching pacing off puts back the limit each game had before.

RTSS and Special K hook the game's present call instead. Anti-cheats let RTSS do that because its
DLLs are signed and on their allow lists; an unsigned DLL in Fortnite is exactly what Easy
Anti-Cheat looks for, and Valve banned CS2 players in 2023 for AMD's Anti-Lag+, which hooked the
game's DLLs. Event tracing and the driver's own limiter touch no game process.

A game is anything presenting from where the stores install games (Steam, Epic, GOG, Xbox, Riot,
EA, Ubisoft, Battle.net), apart from their launchers and tools such as Wallpaper Engine, or
anything whose own window is in the foreground and covers its monitor. Steam and Epic games show
their store name. Displays come from the Windows display configuration (exact refresh rate, the
EDID's VRR range) and G-SYNC state from the NVIDIA driver.

Reading frame times needs an administrator or a member of Performance Log Users. FormalPacer runs
as the user, so the window offers to add you to the group: one consent prompt, then sign out and
back in.

Not done yet on Windows: games measured from outside have no in-game overlay, and AMD and Intel
drivers have no limiter FormalPacer can set, so there those games are measured only.

## Features

- Picks up games by itself: anything Steam or Proton starts, Wine and Lutris games, and any
  `.exe`, 64-bit and 32-bit. Launchers, gamescope and the Steam client are left alone.
- OpenGL games run through `formalpacer run`: `formalpacer run %command%` in Steam's launch
  options. Vulkan games need nothing.
- Steam games show their store name, read from the library's app manifests.
- One process: the engine, the tray item and the window. It starts in the tray at login. Closing
  the window leaves pacing and the tray item running; launching FormalPacer again brings the
  window back.
- The tray item shows the game and its cap, toggles pacing and the overlay, and quits.
- `formalpacer --headless` runs the engine with no window and writes what it does to stderr.
- Follows the system: light and dark, the accent colour, and matugen palettes, live.

Not done yet: a windowed Wayland-native game on KDE or GNOME falls back to the VRR display,
since neither compositor tells another process where a window is (fullscreen ones are found by
their size). OpenGL games are not picked up without `formalpacer run`, since only a Vulkan layer
gets loaded into every game by itself.

## Install

Needs Rust 1.95 (pinned by `rust-toolchain.toml`) with the `i686-unknown-linux-gnu` target for
32-bit games (`rustup target add i686-unknown-linux-gnu`), 32-bit glibc headers (`glibc-devel-32bit`
and `gcc-32bit` on openSUSE, `lib32-glibc` on Arch, `gcc-multilib` on Debian),
[just](https://github.com/casey/just) and, to build, `libxkbcommon`, `wayland`, `vulkan-loader`,
`fontconfig`, `freetype`.

```sh
git clone https://github.com/FormalStudio/FormalPacer
cd FormalPacer
just install      # the app, both layers and the OpenGL hooks, a desktop entry and a login item
```

Start FormalPacer once (after that it starts at login), then any game. Games that were already
running when the layer went in need a restart. If the layer is not registered, the window offers
to register the one installed next to the app. `just uninstall` removes everything `install` put
down.

On Windows, with Rust 1.95 and the Visual Studio build tools:

```powershell
git clone https://github.com/FormalStudio/FormalPacer
cd FormalPacer
powershell -ExecutionPolicy Bypass -File scripts\install-windows.ps1
```

It builds the app and the layer, downloads PresentMon (pinned, hash checked), installs them in
`%LOCALAPPDATA%\Programs\FormalPacer`, registers the layer and starts FormalPacer at sign-in.
`nd package windows` builds a `Setup.exe` with the same files once the script has fetched
PresentMon.

To force the layer on for a game it does not recognise, set `FORMALPACER=1` in its launch options
(`FORMALPACER=1 %command%` in Steam). `FORMALPACER=0` leaves one game alone, and
`DISABLE_FORMALPACER=1` keeps the layer out of a process entirely.

## Configuration

The window writes `~/.config/formalpacer/config.toml` (`%APPDATA%\FormalPacer\config.toml` on
Windows):

```toml
mode = "auto"        # "off" measures without capping
display = "DP-1"     # leave out to follow the game (DISPLAY1 on Windows)
overlay = true
```

On Windows it also keeps `[driver_caps]`: the games whose NVIDIA driver limit FormalPacer has set,
each with the limit it had before.

## Development

```
crates/formalpacer-shm      the shared page between layer and app
crates/formalpacer-hook     what runs in the game: detection, the limiter, the overlay, its hotkey
crates/formalpacer-layer    the Vulkan layer (cdylib): pacing and the overlay by buffer copy
crates/formalpacer-gl       the OpenGL hook (cdylib, LD_PRELOAD): pacing and the overlay by clears
crates/formalpacer-engine   displays, placement, the planner, the engine thread
src/                        the nd app: window, tray item, single instance
tools/fakegame              a Vulkan test game with a set frame rate (--background: unfocused, in a corner)
```

```sh
just dev                          # run from the checkout; FORMALPACER_DEMO=1 opens on a fixed snapshot
nd check                          # fmt, clippy, framework lints, appearance audit
nd test                           # tier 1 and 2
cargo test --workspace            # the planner, placement, EDID and the shared page
just pacing-check 200 100 60      # fakegame at each rate under the headless engine
FAKEGAME_FLAGS=--fullscreen just pacing-check
```

`just pacing-check` registers the layer from the checkout for its own runs only and prints the
rate fakegame measured next to the cap the engine chose.

## License

MIT
