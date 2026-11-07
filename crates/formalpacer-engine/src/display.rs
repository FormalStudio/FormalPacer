//! Displays: the mode each one runs, whether the panel can do VRR and whether it is running right
//! now, which is the honest answer to "is VRR working". On Linux that is the kernel's mode setting
//! state (`VRR_ENABLED` on the CRTC); on Windows the display configuration, and the NVIDIA driver
//! for G-SYNC. The VRR range comes from the EDID on both.

#[cfg(unix)]
mod kms;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use kms::probe;
#[cfg(windows)]
pub use windows::probe;

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
