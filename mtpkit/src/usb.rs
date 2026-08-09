//! USB transport: MTP device discovery and bulk pipe I/O via nusb
//! (pure Rust; IOKit on macOS, usbfs on Linux — no libusb).

use anyhow::{Context as _, Result, bail};
use futures_lite::future::block_on;
use nusb::transfer::{Direction, EndpointType, RequestBuffer};

/// Bulk transfer chunk size. A multiple of every possible USB max packet
/// size, so mid-phase chunks never produce a short packet.
pub const CHUNK: usize = 1 << 20;

/// A USB device exposing an MTP interface, as found by [`list_devices`].
pub struct MtpDevice {
    /// Human-readable name from the USB descriptors (manufacturer + product).
    pub name: String,
    info: nusb::DeviceInfo,
}

struct IfaceDesc {
    number: u8,
    ep_in: u8,
    ep_out: u8,
}

fn debug_enabled() -> bool {
    std::env::var_os("MTPKIT_DEBUG").is_some()
}

/// MTP interfaces come in two shapes: the PTP/still-image class (6/1, any
/// protocol), or a vendor-specific class whose interface string descriptor is
/// named "MTP" (how Android exposes MTP without a mode switch).
fn find_mtp_interface(dev: &nusb::Device) -> Option<IfaceDesc> {
    let config = dev.active_configuration().ok()?;
    for group in config.interfaces() {
        for alt in group.alt_settings() {
            let name = alt
                .string_index()
                .and_then(|i| {
                    dev.get_string_descriptor(i, 0x0409, std::time::Duration::from_millis(500))
                        .ok()
                })
                .unwrap_or_default();
            if debug_enabled() {
                eprintln!(
                    "  interface {}: class {:02x}/{:02x}/{:02x} name {name:?}",
                    group.interface_number(),
                    alt.class(),
                    alt.subclass(),
                    alt.protocol(),
                );
            }
            let is_mtp = (alt.class() == 6 && alt.subclass() == 1) || name.contains("MTP");
            if !is_mtp {
                continue;
            }
            let mut ep_in = None;
            let mut ep_out = None;
            for ep in alt.endpoints() {
                if ep.transfer_type() == EndpointType::Bulk {
                    match ep.direction() {
                        Direction::In => ep_in = Some(ep.address()),
                        Direction::Out => ep_out = Some(ep.address()),
                    }
                }
            }
            if let (Some(ep_in), Some(ep_out)) = (ep_in, ep_out) {
                return Some(IfaceDesc {
                    number: group.interface_number(),
                    ep_in,
                    ep_out,
                });
            }
        }
    }
    None
}

/// Enumerate USB devices and keep those exposing an MTP interface.
///
/// Devices that cannot be opened (hubs, devices captured by other drivers)
/// are silently skipped. Set `MTPKIT_DEBUG=1` to print every device and
/// interface considered.
pub fn list_devices() -> Result<Vec<MtpDevice>> {
    let mut found = Vec::new();
    for info in nusb::list_devices().context("listing USB devices")? {
        if debug_enabled() {
            eprintln!(
                "usb device {:04x}:{:04x} {} {}",
                info.vendor_id(),
                info.product_id(),
                info.manufacturer_string().unwrap_or("?"),
                info.product_string().unwrap_or("?"),
            );
        }
        // Devices we cannot open (hubs, captured devices) are simply not ours.
        let Ok(dev) = info.open() else {
            if debug_enabled() {
                eprintln!("  (cannot open, skipped)");
            }
            continue;
        };
        if find_mtp_interface(&dev).is_none() {
            continue;
        }
        let name = format!(
            "{} {}",
            info.manufacturer_string().unwrap_or(""),
            info.product_string().unwrap_or("(unnamed USB device)"),
        )
        .trim()
        .to_string();
        found.push(MtpDevice { name, info });
    }
    Ok(found)
}

/// A claimed MTP interface with its bulk in/out endpoints — the [`Pipe`]
/// implementation used against real hardware.
///
/// [`Pipe`]: crate::session::Pipe
pub struct Transport {
    iface: nusb::Interface,
    ep_in: u8,
    ep_out: u8,
}

impl Transport {
    /// Open the device and claim its MTP interface exclusively.
    ///
    /// Fails if another program (or a mounted MTP volume) holds the interface.
    pub fn open(device: &MtpDevice) -> Result<Self> {
        let dev = device.info.open().context("opening USB device")?;
        let Some(idesc) = find_mtp_interface(&dev) else {
            bail!("device has no MTP interface (is it in File Transfer mode?)");
        };
        let iface = dev.claim_interface(idesc.number).context(
            "claiming MTP interface — is another program (or a mounted MTP volume) using the device?",
        )?;
        Ok(Self {
            iface,
            ep_in: idesc.ep_in,
            ep_out: idesc.ep_out,
        })
    }

    /// Blocking bulk write of one buffer to the out endpoint.
    pub fn write(&self, data: Vec<u8>) -> Result<()> {
        block_on(self.iface.bulk_out(self.ep_out, data))
            .into_result()
            .context("USB bulk write")?;
        Ok(())
    }

    /// Blocking bulk read from the in endpoint; returns when `max` bytes
    /// arrive or the device ends the transfer with a short packet.
    pub fn read(&self, max: usize) -> Result<Vec<u8>> {
        let data = block_on(self.iface.bulk_in(self.ep_in, RequestBuffer::new(max)))
            .into_result()
            .context("USB bulk read")?;
        Ok(data)
    }
}
