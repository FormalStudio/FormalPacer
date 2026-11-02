pub mod app;
pub mod demo;
mod instance;
mod pump;
mod run;
mod tray;
pub use tray::build as tray_for;
pub mod words;

use formalpacer_engine::engine::{self, Snapshot};
use nd::prelude::*;

pub use app::{Main, Model};
pub use run::exec as run_game;

pub fn app() -> Application {
    Application::new(nd::manifest!())
}

/// Opens the window, or brings it forward when it is already open.
pub fn show(cx: &mut App, model: &Entity<Model>) {
    let existing = cx.window_ids().next();
    if let Some(id) = existing {
        cx.activate_window(id);
        return;
    }
    open(cx, model.clone());
}

pub fn open(cx: &mut App, model: Entity<Model>) -> WindowHandle<Main> {
    cx.open_window(
        WindowOptions::main("main").title("FormalPacer").size(size(px(560.), px(520.))),
        move |_window, cx| cx.new(|cx| Main::new(model, cx)),
    )
}

/// The window on the demo snapshot, for the appearance audit and screenshots.
pub fn open_demo(cx: &mut App) -> WindowHandle<Main> {
    let (commands, _) = flume::unbounded();
    let model = cx.new(|_| {
        let mut model = Model::new(commands);
        model.apply(demo::snapshot());
        model
    });
    open(cx, model)
}

/// The app: one process holding the engine, the tray item and, when open, the window. Closing
/// the window leaves the process running behind the tray item, so games stay paced.
/// `--background` starts it with only the tray item, for login.
pub fn run() -> nd::Result<()> {
    if demo::enabled() {
        return app().run(|cx| {
            open_demo(cx);
        });
    }
    let toggle_only = std::env::args().any(|a| a == instance::TOGGLE_OVERLAY);
    let primary = match instance::claim() {
        Some(instance::Claim::Forwarded) => return Ok(()),
        // Nothing running to toggle.
        Some(instance::Claim::Primary(_)) | None if toggle_only => return Ok(()),
        Some(instance::Claim::Primary(primary)) => Some(primary),
        None => None,
    };
    let background = std::env::args().any(|a| a == "--background");
    app().run(move |cx| {
        cx.set_quit_when_windows_close(false);
        let engine = match engine::start() {
            Ok(engine) => engine,
            Err(e) => {
                nd::report_issue(&format!("could not start the engine: {e}"));
                cx.quit();
                return;
            }
        };
        let model = cx.new(|_| Model::new(engine.commands));
        let tray_model = model.clone();
        pump::pump(engine.snapshots, move |snapshot: Snapshot, cx| {
            tray_model.update(cx, |m, cx| {
                m.apply(snapshot);
                cx.notify();
            });
            tray::sync(&tray_model, cx);
        });
        tray::listen(model.clone());
        if let Some(primary) = primary {
            instance::show_on_launch(primary, model.clone());
        }
        if !background {
            open(cx, model);
        }
    })
}

/// The engine without a window or tray item, writing to stderr whenever the game, its display or
/// the cap changes, and the frame rates every two seconds. For checking the pacing by hand.
pub fn headless() -> std::io::Result<()> {
    let engine = engine::start()?;
    let mut last = String::new();
    let mut shown = 0;
    for (i, snapshot) in engine.snapshots.iter().enumerate() {
        let state = words::state_line(&snapshot);
        if state != last || i - shown >= 8 {
            eprintln!("{state}{}", words::rates_line(&snapshot));
            last = state;
            shown = i;
        }
    }
    Ok(())
}
