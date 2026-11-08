# CLAUDE.md

FormalPacer caps games to a frame rate they can hold on the display they are on. An implicit
Vulkan layer (`crates/formalpacer-layer`, built 64-bit and 32-bit) and an `LD_PRELOAD` OpenGL hook
(`crates/formalpacer-gl`) share the in-game code in `crates/formalpacer-hook`: they sleep before
each present, draw the overlay, and write frame samples to a page in `/dev/shm`
(`crates/formalpacer-shm`, same layout on both targets); the engine thread (`crates/formalpacer-engine`)
reads displays from DRM, asks the compositor or X server which display the game is on, plans the
cap and writes it back. `src/` is the nd app: one process with the engine, the tray item and a
window that can close without stopping pacing (`set_quit_when_windows_close(false)`, single
instance through `nd-ipc`). nd is on `branch = "main"`, pinned by `Cargo.lock`, as in trove.

On Windows the same layer is registered in `HKCU\SOFTWARE\Khronos\Vulkan\ImplicitLayers` and the
page is a mapped file in `%TEMP%\FormalPacer`. Direct3D and OpenGL games get
`crates/formalpacer-inject`, a DLL the engine loads into the running game (`process::inject`)
that hooks the DXGI presents and `wglSwapBuffers` with MinHook and reports through the same page;
the OpenGL overlay is shared with Linux in `formalpacer-hook/src/gl.rs`. Every game under a kernel
anti-cheat (`formalpacer-hook/src/library.rs`), and any game the hook did not reach within five
seconds, is measured from outside by PresentMon (`engine/src/presentmon.rs`, a hidden child
process) and capped live through RTSS when it runs (`rtss.rs`: profile calls in
`RTSSHooks64.dll`, hooked pids from its shared memory), otherwise through the NVIDIA driver
profile (`nvidia.rs`, `driver.rs`), which applies from the game's next launch; its overlay is RTSS's on-screen display when RTSS is
in the game, else a click-through topmost window of ours (`engine/src/floating.rs`), never shown
while VRR runs because it makes Windows compose the game and G-SYNC turns off. Never add anything that loads into an anti-cheat game: the engine
checks before injecting and the DLL checks again. Build there with `RUSTUP_TOOLCHAIN=1.95.0-x86_64-pc-windows-msvc`
(the host default is the GNU toolchain).

The planner's rules and the display order are in the README and in the module docs of
`pacing.rs` and `placement.rs`; change them there together. `just pacing-check` runs
`tools/fakegame` under the headless engine with the checkout's layer and is the end-to-end check
for any change to the layer, the planner or placement.

## Commands

```sh
just dev                  # cargo run; FORMALPACER_DEMO=1 just dev opens on the demo snapshot
just pacing-check         # fakegame at 200, 100, 60 fps under the headless engine
nd check                  # fmt, clippy, framework lints, appearance audit. Run before every commit
nd test                   # tiers 1 and 2, parallel, no window server
nd test --tier real       # tier 3: real windows, queued behind the machine lock
nd diagnose --last        # the newest incident bundle. Read it before reproducing any bug
nd build --target web     # static web bundle in target/nd/web/formalpacer/
nd serve --target web     # build and serve it on localhost:8080
nd check --release        # also fails on README placeholders; run before a release
nd package mac            # .app, .pkg and .dmg (linux: AppImage, windows: Setup.exe)
```

Upgrading nd: `nd upgrade` (or `nd upgrade --to vX.Y.Z`) moves the `nd` tag and the GPUI patch
block together and prints the changelog entries in between. Never edit them by hand. Then
reinstall the CLI from the same tag (`cargo install --git
https://github.com/FormalStudio/NativeDesktop --tag vX.Y.Z --locked nd-cli`) and run `nd check` and
`nd test`.

## Framework rules

`nd check` enforces most of these; the finding names the rule. Details and examples live in
`docs/agents/`.

1. The OS draws its own structure. Use `scaffold`, `toolbar`, `sidebar`, `source_list`,
   `titlebar`, `window_controls`, `window.open_sheet`, `.context_menu`, `window.alert`, `glass`.
   Never a replica. `.drawn()` needs a reason comment. ([patterns](docs/agents/patterns.md))
2. Intents, not `cfg`. Platform tweaks go through `.on_macos(..)`, `.on_windows(..)`,
   `.on_linux(..)`, `.on_web(..)`; capability checks through `nd::platform::has(Feature::..)`.
3. Tokens, not literals. Colours, spacing, radii and type sizes come from `cx.theme()`. Colour
   constructors live in `src/theme.rs` only. ([styling](docs/agents/styling.md))
4. Every window is listed in `tests/appearance.rs`. The audit fails non-token colours and text
   under WCAG contrast in light, dark and matugen.
5. Any list that can pass 50 rows is `list()`/`table()`/`grid()`/`tree()` over a keyed
   `ListState`, with `set_items_versioned`. Never a scroll div of children.
   ([widgets](docs/agents/widgets.md))
6. Controlled values: store what the event reports and render it back. No "skip the next
   change" flags. Native widgets keep a `Synced<T>` and wrap setters in `cx.programmatic(..)`.
7. Floating things are overlays: `popover`, `panel`, `tooltip`, `.context_menu` or a built-in.
8. Time comes from the frame clock (`window.now()`), background work from
   `nd::system::spawn`. No `Instant::now`, `thread::sleep` or `smol::Timer` in UI code.
9. Small views. State lives in entities; `cx.notify()` re-renders one view, so split along the
   lines where state changes. ([performance](docs/agents/performance.md))
10. No inline SVG (use `Icon`), no `gpui`/`objc2` outside native widget modules, no `unwrap`,
    `expect`, `dbg!` or `println!` outside tests.

A finding prints as `file:line:col: [rule] message`. Silence one only with a reason:
`// nd-allow(rule): why` on the line or the line above.

The full don'ts list: [docs/agents/donts.md](docs/agents/donts.md).

## Testing

Test in the cheapest tier that can see the behaviour ([testing](docs/agents/testing.md)):

- Tier 1, `tests/app.rs`: `TestApp` on the recording backend. Logic, actions, focus, list diffs.
  No window, deterministic clock, runs on every OS and in a browser.
- Tier 2, `tests/appearance.rs` and goldens: real backends in process. How it looks.
- Tier 3, `tests/real.rs` or tests named `real_*`: the app as its own process over the
  automation socket. Only for what the OS does: real input, focus, resize, menus.

Tier 3 runs that move the real cursor or capture the screen hold a machine-wide lock. A run in
another worktree queues behind it and prints the holder; that is waiting, not a hang. Check it
with `ndshell lock status`. Never kill the holder or delete the lock file.

## When something breaks

1. `nd diagnose --last`. The bundle has the panic or hang, the last inputs, the semantic tree,
   recent frames and the log tail. Most bugs need no reproduction after it.
2. Write the failing test in the lowest tier that shows it, then fix.
3. When the app detects a bad state itself, call `nd::report_issue("what happened")`; it writes a
   bundle and returns its path.

## Shell and files

- Never `rm` or `mv` a variable path. `rm -rf "$DIR/"` with an empty `DIR` deletes from the
  root. Delete only what your code created, through a helper that refuses empty paths and
  anything outside its root.
- Never `pkill -f <pattern>`; stop processes by the pid you started.
- Other agents may be working in this tree. Never discard changes you did not make.
