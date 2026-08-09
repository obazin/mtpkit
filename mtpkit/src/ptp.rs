//! PTP/MTP wire format: container constants, operation/response codes and
//! little-endian (de)serialization of the datasets mtpkit needs.

use anyhow::{Result, bail};

/// Container type: an operation request (code + parameters).
pub const CONTAINER_COMMAND: u16 = 1;
/// Container type: the payload phase of an operation, in either direction.
pub const CONTAINER_DATA: u16 = 2;
/// Container type: the device's verdict closing a transaction.
pub const CONTAINER_RESPONSE: u16 = 3;

/// Response code: the operation succeeded.
pub const RESP_OK: u16 = 0x2001;
/// Response code returned when a previous client left its session open.
pub const RESP_SESSION_ALREADY_OPEN: u16 = 0x201E;

/// Object format: opaque file content (the device classifies by extension).
pub const FMT_UNDEFINED: u16 = 0x3000;
/// Object format: an association — MTP's name for a folder.
pub const FMT_ASSOCIATION: u16 = 0x3001;

/// Pseudo-handle addressing the root of a storage.
pub const PARENT_ROOT: u32 = 0xFFFF_FFFF;

/// The PTP operation codes mtpkit uses.
pub mod op {
    pub const GET_DEVICE_INFO: u16 = 0x1001;
    pub const OPEN_SESSION: u16 = 0x1002;
    pub const CLOSE_SESSION: u16 = 0x1003;
    pub const GET_STORAGE_IDS: u16 = 0x1004;
    pub const GET_STORAGE_INFO: u16 = 0x1005;
    pub const GET_OBJECT_HANDLES: u16 = 0x1007;
    pub const GET_OBJECT_INFO: u16 = 0x1008;
    pub const GET_OBJECT: u16 = 0x1009;
    pub const DELETE_OBJECT: u16 = 0x100B;
    pub const SEND_OBJECT_INFO: u16 = 0x100C;
    pub const SEND_OBJECT: u16 = 0x100D;
}

/// Human-readable name for a PTP response code, e.g. `InvalidObjectHandle (0x2009)`.
///
/// Unknown codes render as `response 0xXXXX` so nothing is ever swallowed.
pub fn response_name(code: u16) -> String {
    let name = match code {
        0x2001 => "OK",
        0x2002 => "GeneralError",
        0x2003 => "SessionNotOpen",
        0x2004 => "InvalidTransactionID",
        0x2005 => "OperationNotSupported",
        0x2006 => "ParameterNotSupported",
        0x2007 => "IncompleteTransfer",
        0x2008 => "InvalidStorageID",
        0x2009 => "InvalidObjectHandle",
        0x200A => "DevicePropNotSupported",
        0x200C => "StoreFull",
        0x200D => "ObjectWriteProtected",
        0x200E => "StoreReadOnly",
        0x200F => "AccessDenied",
        0x2013 => "StoreNotAvailable",
        0x2014 => "SpecificationByFormatUnsupported",
        0x2015 => "NoValidObjectInfo",
        0x2019 => "DeviceBusy",
        0x201A => "InvalidParentObject",
        0x201D => "InvalidParameter",
        0x201E => "SessionAlreadyOpen",
        0xA801 => "Invalid_ObjectPropCode",
        0xA808 => "Object_Too_Large",
        _ => "",
    };
    if name.is_empty() {
        format!("response 0x{code:04X}")
    } else {
        format!("{name} (0x{code:04X})")
    }
}

/// Little-endian cursor over a PTP dataset.
///
/// Every accessor advances the position and fails cleanly (rather than
/// panicking) when the device sent fewer bytes than the dataset claims.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.buf.len() {
            bail!(
                "truncated PTP dataset: wanted {n} bytes at offset {}, have {}",
                self.pos,
                self.buf.len()
            );
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes(b.try_into().unwrap()))
    }

    /// PTP string: u8 length in UTF-16 units (terminator included), then the units.
    pub fn string(&mut self) -> Result<String> {
        let n = self.u8()? as usize;
        if n == 0 {
            return Ok(String::new());
        }
        let mut units = Vec::with_capacity(n);
        for _ in 0..n {
            units.push(self.u16()?);
        }
        while units.last() == Some(&0) {
            units.pop();
        }
        Ok(String::from_utf16_lossy(&units))
    }

    /// PTP AUINT16: u32 element count, then the elements.
    pub fn array_u16(&mut self) -> Result<Vec<u16>> {
        let n = self.u32()? as usize;
        (0..n).map(|_| self.u16()).collect()
    }

    /// PTP AUINT32: u32 element count, then the elements.
    pub fn array_u32(&mut self) -> Result<Vec<u32>> {
        let n = self.u32()? as usize;
        (0..n).map(|_| self.u32()).collect()
    }
}

/// Little-endian buffer builder for PTP containers and datasets — the
/// serializing counterpart of [`Reader`].
#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }

    /// Encode a PTP string; fails if it exceeds the format's 254-char limit.
    pub fn string(&mut self, s: &str) -> Result<()> {
        if s.is_empty() {
            self.u8(0);
            return Ok(());
        }
        let units: Vec<u16> = s.encode_utf16().collect();
        if units.len() + 1 > 255 {
            bail!("string too long for PTP encoding: {s:?}");
        }
        self.u8((units.len() + 1) as u8);
        for u in units {
            self.u16(u);
        }
        self.u16(0);
        Ok(())
    }
}

/// The subset of the ObjectInfo dataset mtpkit cares about.
///
/// Parsed from `GetObjectInfo` responses, and serialized to announce an
/// upload (`SendObjectInfo`) or create a folder.
pub struct ObjectInfo {
    pub storage_id: u32,
    /// Object format code; [`FMT_ASSOCIATION`] marks a folder.
    pub format: u16,
    /// File size in bytes; `u32::MAX` when the object is 4 GiB or larger.
    pub compressed_size: u32,
    /// Parent folder handle; [`PARENT_ROOT`] at the storage root.
    pub parent: u32,
    /// Association subtype for folders (1 = generic folder).
    pub association_type: u16,
    pub filename: String,
    /// `YYYYMMDDThhmmss`, or empty when the device doesn't report one.
    pub modification_date: String,
}

impl ObjectInfo {
    pub fn is_folder(&self) -> bool {
        self.format == FMT_ASSOCIATION
    }

    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let storage_id = r.u32()?;
        let format = r.u16()?;
        let _protection = r.u16()?;
        let compressed_size = r.u32()?;
        let _thumb_format = r.u16()?;
        let _thumb_size = r.u32()?;
        let _thumb_w = r.u32()?;
        let _thumb_h = r.u32()?;
        let _img_w = r.u32()?;
        let _img_h = r.u32()?;
        let _img_depth = r.u32()?;
        let parent = r.u32()?;
        let association_type = r.u16()?;
        let _association_desc = r.u32()?;
        let _sequence = r.u32()?;
        let filename = r.string()?;
        let _created = r.string()?;
        let modification_date = r.string().unwrap_or_default();
        Ok(Self {
            storage_id,
            format,
            compressed_size,
            parent,
            association_type,
            filename,
            modification_date,
        })
    }

    /// Serialize a dataset suitable for SendObjectInfo.
    pub fn serialize(&self) -> Result<Vec<u8>> {
        let mut w = Writer::new();
        w.u32(self.storage_id);
        w.u16(self.format);
        w.u16(0); // protection
        w.u32(self.compressed_size);
        w.u16(0); // thumb format
        w.u32(0); // thumb size
        w.u32(0); // thumb width
        w.u32(0); // thumb height
        w.u32(0); // image width
        w.u32(0); // image height
        w.u32(0); // image bit depth
        w.u32(self.parent);
        w.u16(self.association_type);
        w.u32(0); // association description
        w.u32(0); // sequence number
        w.string(&self.filename)?;
        w.string("")?; // date created
        w.string(&self.modification_date)?;
        w.string("")?; // keywords
        Ok(w.into_inner())
    }
}

/// Capacity and naming of one storage, from `GetStorageInfo`.
pub struct StorageInfo {
    pub max_capacity: u64,
    pub free_space: u64,
    /// Device-provided description, e.g. "Internal shared storage".
    pub description: String,
    /// User-visible volume label; often empty.
    pub volume_label: String,
}

impl StorageInfo {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let _storage_type = r.u16()?;
        let _fs_type = r.u16()?;
        let _access = r.u16()?;
        let max_capacity = r.u64()?;
        let free_space = r.u64()?;
        let _free_images = r.u32()?;
        let description = r.string()?;
        let volume_label = r.string().unwrap_or_default();
        Ok(Self {
            max_capacity,
            free_space,
            description,
            volume_label,
        })
    }
}

/// Device identity, from `GetDeviceInfo`.
pub struct DeviceInfo {
    pub manufacturer: String,
    pub model: String,
    /// Firmware / software version string.
    pub device_version: String,
    pub serial: String,
}

impl DeviceInfo {
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut r = Reader::new(data);
        let _standard_version = r.u16()?;
        let _vendor_extension_id = r.u32()?;
        let _extension_version = r.u16()?;
        let _extension_desc = r.string()?;
        let _functional_mode = r.u16()?;
        let _operations = r.array_u16()?;
        let _events = r.array_u16()?;
        let _device_props = r.array_u16()?;
        let _capture_formats = r.array_u16()?;
        let _playback_formats = r.array_u16()?;
        let manufacturer = r.string()?;
        let model = r.string()?;
        let device_version = r.string()?;
        let serial = r.string().unwrap_or_default();
        Ok(Self {
            manufacturer,
            model,
            device_version,
            serial,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Reader ----------------------------------------------------------

    #[test]
    fn reader_scalars_are_little_endian() {
        let buf = [
            0x01, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 8, 7, 6, 5, 4, 3, 2, 1,
        ];
        let mut r = Reader::new(&buf);
        assert_eq!(r.u8().unwrap(), 0x01);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.u32().unwrap(), 0x12345678);
        assert_eq!(r.u64().unwrap(), 0x0102030405060708);
    }

    #[test]
    fn reader_errors_on_truncated_input() {
        let mut r = Reader::new(&[0x01]);
        assert!(r.u32().is_err());
        let mut r = Reader::new(&[]);
        assert!(r.u8().is_err());
    }

    #[test]
    fn reader_empty_string() {
        let mut r = Reader::new(&[0]);
        assert_eq!(r.string().unwrap(), "");
    }

    #[test]
    fn reader_string_strips_null_terminator() {
        // "Hi" + NUL, length 3 (terminator included)
        let buf = [3, b'H', 0, b'i', 0, 0, 0];
        let mut r = Reader::new(&buf);
        assert_eq!(r.string().unwrap(), "Hi");
    }

    #[test]
    fn reader_string_errors_when_units_missing() {
        let mut r = Reader::new(&[5, b'H', 0]); // claims 5 units, has 1
        assert!(r.string().is_err());
    }

    #[test]
    fn reader_arrays() {
        let buf = [2, 0, 0, 0, 0x34, 0x12, 0x78, 0x56];
        assert_eq!(Reader::new(&buf).array_u16().unwrap(), vec![0x1234, 0x5678]);
        let buf = [1, 0, 0, 0, 0x78, 0x56, 0x34, 0x12];
        assert_eq!(Reader::new(&buf).array_u32().unwrap(), vec![0x12345678]);
        let buf = [0, 0, 0, 0];
        assert_eq!(Reader::new(&buf).array_u32().unwrap(), Vec::<u32>::new());
    }

    // ---- Writer ----------------------------------------------------------

    #[test]
    fn writer_scalars_roundtrip() {
        let mut w = Writer::new();
        w.u8(0xAB);
        w.u16(0x1234);
        w.u32(0xDEADBEEF);
        w.bytes(&[1, 2]);
        let buf = w.into_inner();
        let mut r = Reader::new(&buf);
        assert_eq!(r.u8().unwrap(), 0xAB);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.u32().unwrap(), 0xDEADBEEF);
    }

    #[test]
    fn writer_string_roundtrips_through_reader() {
        for s in ["", "hello", "héllo wörld", "日本語", "emoji 🚀 ok"] {
            let mut w = Writer::new();
            w.string(s).unwrap();
            let buf = w.into_inner();
            assert_eq!(Reader::new(&buf).string().unwrap(), s, "roundtrip {s:?}");
        }
    }

    #[test]
    fn writer_empty_string_is_one_zero_byte() {
        let mut w = Writer::new();
        w.string("").unwrap();
        assert_eq!(w.into_inner(), vec![0]);
    }

    #[test]
    fn writer_rejects_overlong_strings() {
        let long = "x".repeat(255);
        let mut w = Writer::new();
        assert!(w.string(&long).is_err());
        let max = "x".repeat(254);
        let mut w = Writer::new();
        assert!(w.string(&max).is_ok());
    }

    // ---- response_name ---------------------------------------------------

    #[test]
    fn response_name_known_and_unknown_codes() {
        assert_eq!(response_name(0x2001), "OK (0x2001)");
        assert_eq!(response_name(0x2009), "InvalidObjectHandle (0x2009)");
        assert_eq!(response_name(0x2FFF), "response 0x2FFF");
    }

    // ---- ObjectInfo ------------------------------------------------------

    fn sample_object_info() -> ObjectInfo {
        ObjectInfo {
            storage_id: 0x10001,
            format: FMT_UNDEFINED,
            compressed_size: 123456,
            parent: PARENT_ROOT,
            association_type: 0,
            filename: "réport final.pdf".into(),
            modification_date: "20260809T101500".into(),
        }
    }

    #[test]
    fn object_info_roundtrips() {
        let original = sample_object_info();
        let parsed = ObjectInfo::parse(&original.serialize().unwrap()).unwrap();
        assert_eq!(parsed.storage_id, original.storage_id);
        assert_eq!(parsed.format, original.format);
        assert_eq!(parsed.compressed_size, original.compressed_size);
        assert_eq!(parsed.parent, original.parent);
        assert_eq!(parsed.association_type, original.association_type);
        assert_eq!(parsed.filename, original.filename);
        assert_eq!(parsed.modification_date, original.modification_date);
    }

    #[test]
    fn object_info_folder_detection() {
        let mut info = sample_object_info();
        assert!(!info.is_folder());
        info.format = FMT_ASSOCIATION;
        assert!(info.is_folder());
    }

    #[test]
    fn object_info_parse_rejects_truncated_datasets() {
        let full = sample_object_info().serialize().unwrap();
        assert!(ObjectInfo::parse(&full[..10]).is_err());
    }

    // ---- StorageInfo -----------------------------------------------------

    #[test]
    fn storage_info_parses() {
        let mut w = Writer::new();
        w.u16(3); // storage type
        w.u16(2); // fs type
        w.u16(0); // access
        w.u64(1 << 40);
        w.u64(1 << 39);
        w.u32(0);
        w.string("Internal storage").unwrap();
        w.string("Tablet").unwrap();
        let info = StorageInfo::parse(&w.into_inner()).unwrap();
        assert_eq!(info.max_capacity, 1 << 40);
        assert_eq!(info.free_space, 1 << 39);
        assert_eq!(info.description, "Internal storage");
        assert_eq!(info.volume_label, "Tablet");
    }

    #[test]
    fn storage_info_tolerates_missing_volume_label() {
        let mut w = Writer::new();
        w.u16(3);
        w.u16(2);
        w.u16(0);
        w.u64(100);
        w.u64(50);
        w.u32(0);
        w.string("desc").unwrap();
        // no volume label field at all
        let info = StorageInfo::parse(&w.into_inner()).unwrap();
        assert_eq!(info.volume_label, "");
    }

    // ---- DeviceInfo ------------------------------------------------------

    #[test]
    fn device_info_parses() {
        let mut w = Writer::new();
        w.u16(100); // standard version
        w.u32(6); // vendor extension id
        w.u16(100); // extension version
        w.string("microsoft.com: 1.0; android.com: 1.0").unwrap();
        w.u16(0); // functional mode
        for _ in 0..5 {
            w.u32(1); // one-element arrays: ops, events, props, capture, playback
            w.u16(0x1001);
        }
        w.string("ONYX").unwrap();
        w.string("BOOX Tab").unwrap();
        w.string("4.0").unwrap();
        w.string("SN12345").unwrap();
        let info = DeviceInfo::parse(&w.into_inner()).unwrap();
        assert_eq!(info.manufacturer, "ONYX");
        assert_eq!(info.model, "BOOX Tab");
        assert_eq!(info.device_version, "4.0");
        assert_eq!(info.serial, "SN12345");
    }

    #[test]
    fn device_info_rejects_garbage() {
        assert!(DeviceInfo::parse(&[1, 2, 3]).is_err());
    }
}
