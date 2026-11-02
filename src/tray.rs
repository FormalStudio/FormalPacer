//! The tray item: what is being paced at a glance, the pacing switch, and the way back to the
//! window. It is the app's only surface while the window is closed.

use std::cell::RefCell;

use formalpacer_engine::config::Mode;
use formalpacer_engine::engine::{Command, Snapshot};
use nd::platform::Feature;
use nd::prelude::*;
use nd::system::{SystemEvent, Tray, TrayIcon, TrayMenuItem};

use crate::{Model, words};

const ID: &str = "formalpacer";

thread_local! {
    /// The item as last sent, so a snapshot that changes only frame rates sends nothing.
    static SHOWN: RefCell<Option<Tray>> = const { RefCell::new(None) };
}

pub fn build(snapshot: &Snapshot) -> Tray {
    let status = |label: String| TrayMenuItem::Item { id: "status".into(), label, enabled: false, checked: None };
    let mut tray = Tray::new(ID)
        .icon(TrayIcon::Png { bytes: include_bytes!("../assets/tray.png").to_vec(), template: false })
        .title("FormalPacer");
    match &snapshot.game {
        Some(game) => {
            let cap = words::cap(snapshot, game);
            tray = tray.tooltip(format!("{}: {cap}", game.name)).item(status(game.name.clone())).item(status(cap));
        }
        None => tray = tray.tooltip("No game running").item(status("No game running".into())),
    }
    tray.item(TrayMenuItem::Separator)
        .item(TrayMenuItem::check("auto", "Pace Games", snapshot.config.mode == Mode::Auto))
        .item(TrayMenuItem::item("open", "Open FormalPacer"))
        .item(TrayMenuItem::Separator)
        .item(TrayMenuItem::item("quit", "Quit"))
}

pub fn sync(model: &Entity<Model>, cx: &mut App) {
    if !nd::platform::has(Feature::Tray) {
        return;
    }
    let Some(snapshot) = &model.read(cx).snapshot else { return };
    let tray = build(snapshot);
    let changed = SHOWN.with(|shown| {
        let mut shown = shown.borrow_mut();
        let changed = shown.as_ref() != Some(&tray);
        *shown = Some(tray.clone());
        changed
    });
    if changed {
        nd::system::spawn(nd::system::system().tray().set(tray), |result, _| {
            if let Err(e) = result {
                nd::report_issue(&format!("tray item: {e}"));
            }
        });
    }
}

pub fn listen(model: Entity<Model>) {
    nd::system::on_event(&nd::system::system(), move |event, cx| {
        let SystemEvent::Tray { item, .. } = event else { return };
        match item.as_deref() {
            None | Some("open") => crate::show(cx, &model),
            Some("auto") => {
                let model = model.read(cx);
                let mode = match model.snapshot.as_ref().map(|s| s.config.mode) {
                    Some(Mode::Auto) => Mode::Off,
                    _ => Mode::Auto,
                };
                let _ = model.commands.send(Command::SetMode(mode));
            }
            Some("quit") => cx.quit(),
            _ => {}
        }
    });
}
