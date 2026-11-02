//! A fixed snapshot: a game on a 144 Hz panel with VRR off, held at a third of the refresh
//! rate. Used by the appearance audit, the tier 1 tests and the README screenshots
//! (`FORMALPACER_DEMO=1`).

use formalpacer_engine::config::Config;
use formalpacer_engine::display::{Display, Vrr};
use formalpacer_engine::engine::{Game, Placement, Snapshot};
use formalpacer_engine::layer::LayerState;
use formalpacer_engine::pacing::{Cap, CapKind, Stats};

/// `FORMALPACER_DEMO=1` opens the window on this snapshot, with no engine or tray item.
pub fn enabled() -> bool {
    std::env::var_os("FORMALPACER_DEMO").is_some()
}

pub fn snapshot() -> Snapshot {
    Snapshot {
        config: Config::default(),
        layer: LayerState::Installed("/usr/lib/formalpacer/libformalpacer_layer.so".into()),
        displays: vec![
            Display {
                connector: "DP-1".into(),
                model: Some("Desk monitor".into()),
                width: 2560,
                height: 1440,
                refresh_hz: 143.973,
                max_refresh_hz: 143.973,
                vrr: Vrr::Off,
                vrr_range: Some((48., 144.)),
            },
            Display {
                connector: "HDMI-A-1".into(),
                model: Some("TV".into()),
                width: 3840,
                height: 2160,
                refresh_hz: 60.,
                max_refresh_hz: 120.,
                vrr: Vrr::Unsupported,
                vrr_range: None,
            },
        ],
        game: Some(Game {
            name: "ELDEN RING".into(),
            steam_app_id: 1245620,
            stats: Stats { avg_fps: 48., low_fps: 46.9, capacity_fps: 61.4, frames: 0, span_s: 0. },
            cap: Some(Cap { fps: 143.973 / 3., kind: CapKind::Fraction(3) }),
            display: "DP-1".into(),
            placement: Placement::Window,
        }),
        notice: None,
    }
}
