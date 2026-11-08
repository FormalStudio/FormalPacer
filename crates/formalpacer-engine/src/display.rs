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

/// The panel's VRR window: from the HDMI Forum block (HDMI 2.1 VRR), else AMD's FreeSync block,
/// else the display range limits descriptor. The descriptor lists every rate the panel takes,
/// which can reach far under its VRR window (24-144 Hz on a panel whose window is 48-144), so it
/// comes last.
pub fn edid_vrr_range(edid: &[u8]) -> Option<(f64, f64)> {
    let limits = range_limits(edid);
    let vendor = |oui: [u8; 3]| cta_blocks(edid).find(|b| b[0] >> 5 == 3 && b.get(1..4) == Some(&oui[..]));
    // VRRmin in the low six bits of payload byte 8, VRRmax as two bits over byte 9; a VRRmax of
    // 0 means the panel's highest rate.
    let hdmi = vendor([0xD8, 0x5D, 0xC4]).and_then(|b| {
        let (lo, hi) = (*b.get(9)?, *b.get(10)?);
        let max = u16::from(lo & 0xC0) << 2 | u16::from(hi);
        Some((u16::from(lo & 0x3F), if max == 0 { limits.map_or(0, |l| l.1) } else { max }))
    });
    let freesync = vendor([0x1A, 0x00, 0x00]).and_then(|b| Some((u16::from(*b.get(6)?), u16::from(*b.get(7)?))));
    [hdmi, freesync, limits]
        .into_iter()
        .flatten()
        .find(|&(min, max)| min > 0 && max > min)
        .map(|(min, max)| (f64::from(min), f64::from(max)))
}

/// The display range limits descriptor (tag 0xFD). Byte 4 holds the +255 offset flags for rates
/// above 255 Hz.
fn range_limits(edid: &[u8]) -> Option<(u16, u16)> {
    let d = descriptors(edid).find(|d| d[3] == 0xFD)?;
    let min = u16::from(d[5]) + if d[4] & 0x01 != 0 { 255 } else { 0 };
    let max = u16::from(d[6]) + if d[4] & 0x02 != 0 { 255 } else { 0 };
    Some((min, max))
}

/// The data blocks of every CTA-861 extension, each with its header byte.
fn cta_blocks(edid: &[u8]) -> impl Iterator<Item = &[u8]> {
    edid.chunks_exact(128).skip(1).filter(|x| x[0] == 0x02).flat_map(|x| {
        let end = usize::from(x[2]).clamp(4, 127);
        let mut at = 4;
        std::iter::from_fn(move || {
            let block = x.get(at..(at + 1 + usize::from(x.get(at)? & 0x1F)).min(end)).filter(|_| at < end)?;
            at += block.len();
            Some(block)
        })
    })
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

        // Range limits of 24-144 Hz, and VRR blocks that say 48-144.
        let mut edid = edid_with(range);
        edid[54 + 5] = 24;
        edid[54 + 6] = 144;
        edid[54 + 4] = 0;
        edid[126] = 1;
        let mut cta = vec![0u8; 128];
        cta[0] = 0x02;
        cta[1] = 0x03;
        let hdmi = [0x6A, 0xD8, 0x5D, 0xC4, 0x01, 0x78, 0x80, 0x00, 0x00, 48, 144];
        let freesync = [0x68, 0x1A, 0x00, 0x00, 0x01, 0x01, 40, 144, 0x00];
        let blocks: Vec<u8> = hdmi.iter().chain(&freesync).copied().collect();
        cta[2] = 4 + blocks.len() as u8;
        cta[4..4 + blocks.len()].copy_from_slice(&blocks);
        edid.extend(&cta);
        assert_eq!(edid_vrr_range(&edid), Some((48., 144.)));
        // Without the HDMI Forum block, FreeSync's window; without either, the range limits.
        edid[128 + 5] = 0x00;
        assert_eq!(edid_vrr_range(&edid), Some((40., 144.)));
        edid[128] = 0;
        assert_eq!(edid_vrr_range(&edid), Some((24., 144.)));

        let mut name = [0u8; 18];
        name[3] = 0xFC;
        name[5..18].copy_from_slice(b"XG27AQM\n     ");
        assert_eq!(edid_name(&edid_with(name)).as_deref(), Some("XG27AQM"));
    }
}
