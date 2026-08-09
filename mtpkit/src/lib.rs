//! mtpkit — a pure-Rust MTP (Media Transfer Protocol) client library.
//!
//! Talks to Android devices (phones, tablets, e-readers, …) over USB without
//! libusb, kernel drivers, or FUSE: [`usb`] discovers devices and provides
//! the bulk transport (via nusb — IOKit on macOS, usbfs on Linux), [`ptp`]
//! implements the PTP/MTP wire format, and [`session`] exposes the
//! high-level operations (browse, upload, download, mkdir, delete) with
//! streaming transfers and progress callbacks.
//!
//! # Example
//!
//! ```no_run
//! use mtpkit::{session::Session, usb};
//!
//! # fn main() -> anyhow::Result<()> {
//! let devices = usb::list_devices()?;
//! let transport = usb::Transport::open(&devices[0])?;
//! let mut session = Session::open(transport)?;
//!
//! for id in session.storage_ids()? {
//!     let info = session.storage_info(id)?;
//!     println!("{}: {} bytes free", info.description, info.free_space);
//! }
//!
//! let entries = session.list(0x10001, mtpkit::ptp::PARENT_ROOT)?;
//! for entry in entries {
//!     println!("{}{}", entry.name, if entry.is_dir { "/" } else { "" });
//! }
//! # Ok(())
//! # }
//! ```
//!
//! # Testing
//!
//! [`session::Session`] is generic over the [`session::Pipe`] trait
//! (defaulting to the real USB [`usb::Transport`]), which is the crate's
//! testing seam: protocol logic can be driven by a scripted mock transport,
//! no hardware required.

pub mod ptp;
pub mod session;
pub mod usb;
