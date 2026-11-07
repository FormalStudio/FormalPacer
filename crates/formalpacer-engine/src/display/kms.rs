//! The kernel's mode setting state, for every lit display on every card.

use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;

use drm::control::{Device as ControlDevice, connector, property};

use super::{Display, Vrr, edid_name, edid_vrr_range};

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}
impl ControlDevice for Card {}

/// Every lit display on every card.
pub fn probe() -> Vec<Display> {
    let Ok(dir) = std::fs::read_dir("/dev/dri") else { return Vec::new() };
    let mut cards: Vec<_> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("card")))
        .collect();
    cards.sort();
    cards.iter().flat_map(|p| probe_card(p)).collect()
}

fn probe_card(path: &Path) -> Vec<Display> {
    let Ok(file) = File::options().read(true).write(true).open(path).or_else(|_| File::open(path)) else {
        return Vec::new();
    };
    let card = Card(file);
    let card_name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_owned();
    let Ok(res) = card.resource_handles() else { return Vec::new() };
    let mut out = Vec::new();
    for &handle in res.connectors() {
        let Ok(info) = card.get_connector(handle, false) else { continue };
        if info.state() != connector::State::Connected {
            continue;
        }
        let Some(crtc) = info.current_encoder().and_then(|e| card.get_encoder(e).ok()).and_then(|e| e.crtc()) else {
            continue;
        };
        let Ok(crtc_info) = card.get_crtc(crtc) else { continue };
        let Some(mode) = crtc_info.mode() else { continue };
        let connector = format!("{}-{}", info.interface().as_str(), info.interface_id());
        let (width, height) = mode.size();
        let max_refresh_hz =
            info.modes().iter().filter(|m| m.size() == mode.size()).map(exact_rate).fold(exact_rate(&mode), f64::max);
        let capable = prop(&card, handle, "vrr_capable").unwrap_or(0) != 0;
        let enabled = prop(&card, crtc, "VRR_ENABLED").unwrap_or(0) != 0;
        let edid = std::fs::read(format!("/sys/class/drm/{card_name}-{connector}/edid")).unwrap_or_default();
        out.push(Display {
            model: edid_name(&edid),
            vrr_range: edid_vrr_range(&edid),
            connector,
            width,
            height,
            refresh_hz: exact_rate(&mode),
            max_refresh_hz,
            vrr: match (capable, enabled) {
                (_, true) => Vrr::Active,
                (true, false) => Vrr::Off,
                (false, false) => Vrr::Unsupported,
            },
        });
    }
    out
}

fn exact_rate(mode: &drm::control::Mode) -> f64 {
    let (_, _, htotal) = mode.hsync();
    let (_, _, vtotal) = mode.vsync();
    if htotal == 0 || vtotal == 0 {
        return f64::from(mode.vrefresh());
    }
    f64::from(mode.clock()) * 1000. / (f64::from(htotal) * f64::from(vtotal))
}

fn prop<T: drm::control::ResourceHandle>(card: &Card, handle: T, name: &str) -> Option<u64> {
    let props = card.get_properties(handle).ok()?;
    let (ids, values) = props.as_props_and_values();
    ids.iter().zip(values).find_map(|(&id, &value): (&property::Handle, &u64)| {
        let info = card.get_property(id).ok()?;
        (info.name().to_bytes() == name.as_bytes()).then_some(value)
    })
}
