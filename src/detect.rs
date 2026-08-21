//! Auto-detection of the scanner's serial device.
//!
//! The UBC125XLT has a built-in USB interface with the fixed id
//! 1965:0018 (Uniden Corp. / UBC125XLT). Detection scans `/sys/class/tty`
//! for `ttyACM*` and `ttyUSB*` entries, resolves each one to its USB
//! device, and matches on that id. No probing: nothing is opened or
//! written, so a failed detection cannot disturb the scanner.

use std::path::Path;

/// USB id of the UBC125XLT's built-in interface (Uniden Corp.).
const UBC125_USB_VENDOR: &str = "1965";
const UBC125_USB_PRODUCT: &str = "0018";

/// How many sysfs levels up from the tty node to look for the USB device
/// directory (worst case: tty dir -> interface -> usb device).
const MAX_ANCESTORS: usize = 5;

/// Errors from scanner device detection.
#[derive(Debug, thiserror::Error)]
pub enum DetectError {
    #[error(
        "no UBC125 (USB {UBC125_USB_VENDOR}:{UBC125_USB_PRODUCT}) serial device found \
         (saw: {seen}); pass --device to specify the port"
    )]
    NotFound { seen: String },

    #[error("multiple UBC125 serial devices found ({devices}); pass --device to pick one")]
    Multiple { devices: String },

    #[error("failed to read sysfs: {0}")]
    Io(#[from] std::io::Error),
}

/// The scanner's serial device path: the explicit one if given, otherwise
/// the auto-detected UBC125.
pub fn resolve_device(explicit: Option<&str>) -> Result<String, DetectError> {
    match explicit {
        Some(device) if !device.is_empty() => Ok(device.to_string()),
        _ => detect_device(),
    }
}

/// Find the UBC125's serial port on this system.
pub fn detect_device() -> Result<String, DetectError> {
    detect(Path::new("/sys"))
}

/// Detection against a sysfs root (the real `/sys`, or a fake tree in tests).
fn detect(sysfs: &Path) -> Result<String, DetectError> {
    let mut matches = Vec::new();
    let mut seen = Vec::new();
    for name in tty_candidates(sysfs)? {
        match usb_identity(sysfs, &name) {
            Some(id) if id.vendor == UBC125_USB_VENDOR && id.product == UBC125_USB_PRODUCT => {
                matches.push(format!("/dev/{name}"));
            }
            Some(id) => seen.push(format!("{name} ({})", id.describe())),
            None => seen.push(format!("{name} (not a USB device)")),
        }
    }
    match matches.len() {
        1 => {
            let device = matches.pop().unwrap();
            tracing::info!(device = %device, "auto-detected scanner device");
            Ok(device)
        }
        0 => Err(DetectError::NotFound {
            seen: if seen.is_empty() { "none".into() } else { seen.join(", ") },
        }),
        _ => Err(DetectError::Multiple {
            devices: matches.join(", "),
        }),
    }
}

/// Tty names in `<sysfs>/class/tty` that can carry a USB serial device,
/// in priority order: `ttyACM*` before `ttyUSB*`, numeric within each group.
fn tty_candidates(sysfs: &Path) -> Result<Vec<String>, DetectError> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(sysfs.join("class/tty"))? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.retain(|n| candidate_rank(n).0 < u8::MAX);
    names.sort_by_key(|n| candidate_rank(n));
    Ok(names)
}

/// Sort key: ACM before USB, numeric suffix within each group,
/// non-candidates last.
fn candidate_rank(name: &str) -> (u8, u64) {
    for (group, prefix) in [(0u8, "ttyACM"), (1u8, "ttyUSB")] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return (group, rest.parse().unwrap_or(u64::MAX));
        }
    }
    (u8::MAX, 0)
}

/// USB identity of the device backing a tty, if it is a USB device.
fn usb_identity(sysfs: &Path, tty_name: &str) -> Option<UsbIdentity> {
    let link = sysfs.join("class/tty").join(tty_name);
    let tty_dir = std::fs::canonicalize(link).ok()?;
    let mut dir = tty_dir.parent()?;
    for _ in 0..MAX_ANCESTORS {
        if dir.join("idVendor").is_file() {
            return Some(UsbIdentity {
                vendor: sysfs_attr(dir, "idVendor"),
                product: sysfs_attr(dir, "idProduct"),
                name: sysfs_attr(dir, "product"),
            });
        }
        dir = dir.parent()?;
    }
    None
}

fn sysfs_attr(dir: &Path, attr: &str) -> String {
    std::fs::read_to_string(dir.join(attr))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

struct UsbIdentity {
    vendor: String,
    product: String,
    name: String,
}

impl UsbIdentity {
    fn describe(&self) -> String {
        if self.name.is_empty() {
            format!("{}:{}", self.vendor, self.product)
        } else {
            format!("{}:{} {}", self.vendor, self.product, self.name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fake sysfs USB device with a tty class link into it,
    /// mirroring the real CDC layout:
    ///   devices/usb/1-1/<port>/            <- USB device (idVendor, ...)
    ///   devices/usb/1-1/<port>/<port>:1.0/ <- interface
    ///   devices/usb/1-1/<port>/<port>:1.0/tty/<tty>  <- tty dir
    ///   class/tty/<tty> -> (symlink to the tty dir)
    fn make_usb_tty(
        root: &Path,
        port: &str,
        tty: &str,
        vendor: &str,
        product: &str,
        name: &str,
    ) {
        let usb_dev = root.join("devices").join("usb").join("1-1").join(port);
        let tty_dir = usb_dev.join(format!("{port}:1.0/tty/{tty}"));
        std::fs::create_dir_all(&tty_dir).unwrap();
        std::fs::write(usb_dev.join("idVendor"), vendor).unwrap();
        std::fs::write(usb_dev.join("idProduct"), product).unwrap();
        std::fs::write(usb_dev.join("product"), name).unwrap();
        let class = root.join("class/tty");
        std::fs::create_dir_all(&class).unwrap();
        std::os::unix::fs::symlink(&tty_dir, class.join(tty)).unwrap();
    }

    /// A tty whose sysfs path has no USB device ancestor (e.g. platform UART).
    fn make_plain_tty(root: &Path, tty: &str) {
        let tty_dir = root.join("devices").join("platform").join(tty);
        std::fs::create_dir_all(&tty_dir).unwrap();
        let class = root.join("class/tty");
        std::fs::create_dir_all(&class).unwrap();
        std::os::unix::fs::symlink(&tty_dir, class.join(tty)).unwrap();
    }

    fn empty_sysfs() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("class/tty")).unwrap();
        tmp
    }

    #[test]
    fn picks_scanner_by_usb_id() {
        let tmp = empty_sysfs();
        make_usb_tty(tmp.path(), "1-1.2", "ttyACM0", "04d8", "0003", "RPi CM4");
        make_usb_tty(tmp.path(), "1-1.3", "ttyACM1", "1965", "0018", "UBC125XLT");
        assert_eq!(detect(tmp.path()).unwrap(), "/dev/ttyACM1");
    }

    #[test]
    fn finds_scanner_on_ttyusb_layout() {
        // USB-serial drivers attach the tty one level shallower (no `tty/` dir).
        let tmp = empty_sysfs();
        let usb_dev = tmp.path().join("devices").join("usb").join("1-1").join("1-1.3");
        let tty_dir = usb_dev.join("1-1.3:1.0/ttyUSB0");
        std::fs::create_dir_all(&tty_dir).unwrap();
        std::fs::write(usb_dev.join("idVendor"), "1965").unwrap();
        std::fs::write(usb_dev.join("idProduct"), "0018").unwrap();
        std::fs::write(usb_dev.join("product"), "UBC125XLT").unwrap();
        std::os::unix::fs::symlink(
            &tty_dir,
            tmp.path().join("class/tty/ttyUSB0"),
        )
        .unwrap();
        assert_eq!(detect(tmp.path()).unwrap(), "/dev/ttyUSB0");
    }

    #[test]
    fn no_candidates_is_not_found() {
        let tmp = empty_sysfs();
        let err = detect(tmp.path()).unwrap_err();
        assert!(matches!(err, DetectError::NotFound { .. }));
        assert!(err.to_string().contains("saw: none"), "{err}");
    }

    #[test]
    fn non_usb_tty_is_ignored_and_listed() {
        let tmp = empty_sysfs();
        make_plain_tty(tmp.path(), "ttyACM0");
        let err = detect(tmp.path()).unwrap_err();
        assert!(matches!(err, DetectError::NotFound { .. }));
        assert!(err.to_string().contains("ttyACM0 (not a USB device)"), "{err}");
    }

    #[test]
    fn multiple_scanners_error_lists_priority_order() {
        let tmp = empty_sysfs();
        // Numeric (not lexicographic) order within the ACM group, ACM before USB.
        make_usb_tty(tmp.path(), "1-1.4", "ttyACM10", "1965", "0018", "UBC125XLT");
        make_usb_tty(tmp.path(), "1-1.2", "ttyACM2", "1965", "0018", "UBC125XLT");
        make_usb_tty(tmp.path(), "1-1.3", "ttyUSB0", "1965", "0018", "UBC125XLT");
        let err = detect(tmp.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("/dev/ttyACM2, /dev/ttyACM10, /dev/ttyUSB0"), "{msg}");
    }

    #[test]
    fn not_found_lists_non_matching_acm_in_numeric_order() {
        let tmp = empty_sysfs();
        make_usb_tty(tmp.path(), "1-1.4", "ttyACM10", "04d8", "0003", "RPi CM4");
        make_usb_tty(tmp.path(), "1-1.2", "ttyACM2", "10c4", "ea60", "CP2102");
        let err = detect(tmp.path()).unwrap_err();
        let msg = err.to_string();
        let i2 = msg.find("ttyACM2").unwrap();
        let i10 = msg.find("ttyACM10").unwrap();
        assert!(i2 < i10, "{msg}");
    }

    #[test]
    fn resolve_device_uses_explicit_path() {
        assert_eq!(
            resolve_device(Some("/dev/ttyACM0")).unwrap(),
            "/dev/ttyACM0"
        );
    }
}
