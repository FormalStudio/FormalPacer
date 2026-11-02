//! Displays from the kernel's mode setting state: the mode each one runs, whether the panel can
//! do VRR and whether the compositor has it switched on right now (`VRR_ENABLED` on the CRTC),
//! which is the honest answer to "is VRR working". The VRR range comes from the EDID.

use std::fs::File;
use std::os::fd::{AsFd, BorrowedFd};
use std::path::Path;

use drm::control::{Device as ControlDevice, connector, property};

struct Card(File);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}

impl drm::Device for Card {}
impl ControlDevice for Card {}

#[derive(Clone, Debug, PartialEq)]
pub enum Vrr {
    Unsupported,
    /// The panel can, the compositor has it off for now.
    Off,
    Active,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    /// `DP-3`, `HDMI-A-1`.
    pub connector: String,
    /// The monitor's own name from its EDID, when it has one.
    pub model: Option<String>,
    pub width: u16,
    pub height: u16,
    /// The current mode's exact rate, from its pixel clock.
    pub refresh_hz: f64,
    /// The fastest mode at the current resolution.
    pub max_refresh_hz: f64,
    pub vrr: Vrr,
    /// The panel's VRR window in Hz.
    pub vrr_range: Option<(f64, f64)>,
}

impl Display {
    pub fn label(&self) -> String {
        match &self.model {
            Some(model) => format!("{model} ({})", self.connector),
            None => self.connector.clone(),
        }
    }

    pub fn vrr_active(&self) -> bool {
        self.vrr == Vrr::Active
    }
}

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

/// The four 18-byte descriptors of the EDID base block.
fn descriptors(edid: &[u8]) -> impl Iterator<Item = &[u8]> {
    [54usize, 72, 90, 108].into_iter().filter_map(|at| edid.get(at..at + 18)).filter(|d| d[..3] == [0, 0, 0])
}

pub fn edid_name(edid: &[u8]) -> Option<String> {
    let d = descriptors(edid).find(|d| d[3] == 0xFC)?;
    let name: String = d[5..].iter().take_while(|&&b| b != 0x0A).map(|&b| b as char).collect();
    let name = name.trim().to_owned();
    (!name.is_empty()).then_some(name)
}

/// The display range limits descriptor (tag 0xFD). Byte 4 holds the +255 offset flags for rates
/// above 255 Hz.
pub fn edid_vrr_range(edid: &[u8]) -> Option<(f64, f64)> {
    let d = descriptors(edid).find(|d| d[3] == 0xFD)?;
    let min = u16::from(d[5]) + if d[4] & 0x01 != 0 { 255 } else { 0 };
    let max = u16::from(d[6]) + if d[4] & 0x02 != 0 { 255 } else { 0 };
    (min > 0 && max > min).then_some((f64::from(min), f64::from(max)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edid_with(descriptor: [u8; 18]) -> Vec<u8> {
        let mut edid = vec![0u8; 128];
        edid[54..72].copy_from_slice(&descriptor);
        edid
    }

    #[test]
    fn reads_range_and_name() {
        let mut range = [0u8; 18];
        range[3] = 0xFD;
        range[5] = 48;
        range[6] = 144;
        assert_eq!(edid_vrr_range(&edid_with(range)), Some((48., 144.)));
        range[4] = 0x02;
        range[6] = 15;
        assert_eq!(edid_vrr_range(&edid_with(range)), Some((48., 270.)));

        let mut name = [0u8; 18];
        name[3] = 0xFC;
        name[5..18].copy_from_slice(b"XG27AQM\n     ");
        assert_eq!(edid_name(&edid_with(name)).as_deref(), Some("XG27AQM"));
    }
}
