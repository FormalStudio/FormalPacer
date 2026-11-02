//! Tier 1: the window on `TestBackend` over the demo snapshot. No window server.

use formalpacer::{Main, Model, demo};
use formalpacer_engine::config::Mode;
use formalpacer_engine::engine::Command;
use formalpacer_engine::layer::LayerState;
use nd::prelude::*;
use nd::test::{SemanticAction, Size, TestApp};

struct Rig {
    cx: TestApp,
    model: Entity<Model>,
    commands: flume::Receiver<Command>,
    window: nd::core::WindowId,
}

fn open(snapshot: formalpacer_engine::engine::Snapshot) -> Rig {
    let mut cx = TestApp::new();
    let (tx, commands) = flume::unbounded();
    let model = cx.new(|_| {
        let mut model = Model::new(tx);
        model.apply(snapshot);
        model
    });
    let m = model.clone();
    let handle = cx.open_window(Size { width: 560., height: 900. }, move |_, cx| cx.new(|cx| Main::new(m, cx)));
    let window = handle.id();
    cx.run_until_idle();
    Rig { cx, model, commands, window }
}

impl Rig {
    fn text(&mut self) -> String {
        nd::test::text(&mut self.cx, self.window)
    }

    fn act(&mut self, id: &str, action: SemanticAction) {
        let node = nd::test::by_test_id(&mut self.cx, self.window, id).unwrap_or_else(|| panic!("no node {id}"));
        self.cx.perform(node.id, &action).unwrap_or_else(|e| panic!("{id}: {e:?}"));
        self.cx.run_until_idle();
    }
}

#[test]
fn shows_the_game_its_cap_and_its_display() {
    let mut rig = open(demo::snapshot());
    let text = rig.text();
    for wanted in [
        "ELDEN RING",
        "48 fps, a third of 144 Hz",
        "Desk monitor on DP-1",
        "143.97 Hz",
        "Supported, off, 48–144 Hz",
        "The game's window is on it",
    ] {
        assert!(text.contains(wanted), "missing {wanted:?} in {text}");
    }
    assert!(!text.contains("Register"), "the layer is installed in the demo");
}

#[test]
fn mode_and_display_choices_go_to_the_engine() {
    let mut rig = open(demo::snapshot());
    rig.act("mode", SemanticAction::SetValue("Off".into()));
    assert_eq!(rig.commands.try_recv().ok(), Some(Command::SetMode(Mode::Off)));
    rig.act("display", SemanticAction::SetValue("TV on HDMI-A-1".into()));
    assert_eq!(rig.commands.try_recv().ok(), Some(Command::SetDisplay(Some("HDMI-A-1".into()))));
    rig.act("display", SemanticAction::SetValue("Follow the game".into()));
    assert_eq!(rig.commands.try_recv().ok(), Some(Command::SetDisplay(None)));
    rig.act("overlay", SemanticAction::Click);
    assert_eq!(rig.commands.try_recv().ok(), Some(Command::SetOverlay(false)));
}

#[test]
fn a_missing_layer_offers_to_register_it() {
    let mut snapshot = demo::snapshot();
    snapshot.layer = LayerState::Missing;
    snapshot.game = None;
    let mut rig = open(snapshot);
    let text = rig.text();
    assert!(text.contains("No game running"), "{text}");
    assert!(text.contains("isn't registered"), "{text}");
    rig.act("layer.action", SemanticAction::Click);
    assert_eq!(rig.commands.try_recv().ok(), Some(Command::InstallLayer));
}

#[test]
fn a_registration_notice_stays_until_dismissed() {
    let mut snapshot = demo::snapshot();
    snapshot.notice = Some("Layer registered".into());
    let mut rig = open(snapshot);
    rig.model.update(&mut rig.cx, |m, cx| {
        m.apply(demo::snapshot());
        cx.notify();
    });
    rig.cx.run_until_idle();
    assert!(rig.text().contains("Layer registered"));
    rig.act("notice.action", SemanticAction::Click);
    assert!(!rig.text().contains("Layer registered"));
}

#[test]
fn the_tray_says_what_is_paced() {
    let tray = formalpacer::tray_for(&demo::snapshot());
    assert_eq!(tray.tooltip.as_deref(), Some("ELDEN RING: 48 fps, a third of 144 Hz"));
}
