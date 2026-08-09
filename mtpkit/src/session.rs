//! MTP session: transaction plumbing (command / data / response phases) and
//! the high-level operations clients use.

use std::io::Read;

use anyhow::{Result, bail};

use crate::ptp::{self, DeviceInfo, ObjectInfo, Reader, StorageInfo, Writer, op, response_name};
use crate::usb::{CHUNK, Transport};

/// Bulk pipe abstraction over the USB transport, so protocol logic can be
/// exercised against a scripted mock in tests.
pub trait Pipe {
    /// Send one buffer as a single bulk transfer.
    fn write(&self, data: Vec<u8>) -> Result<()>;
    /// Receive up to `max` bytes; a short result means the device ended the
    /// transfer early.
    fn read(&self, max: usize) -> Result<Vec<u8>>;
}

impl Pipe for Transport {
    fn write(&self, data: Vec<u8>) -> Result<()> {
        Transport::write(self, data)
    }

    fn read(&self, max: usize) -> Result<Vec<u8>> {
        Transport::read(self, max)
    }
}

/// A directory entry (file or folder) on the device, as returned by
/// [`Session::list`].
#[derive(Clone)]
pub struct Entry {
    /// The object handle — the device's identifier for this file or folder.
    pub handle: u32,
    pub name: String,
    pub is_dir: bool,
    /// Size in bytes; clamped to `u32::MAX` by MTP for files of 4 GiB or more.
    pub size: u64,
}

/// An open MTP session over a bulk pipe.
///
/// Created with [`Session::open`]; the session is closed politely on drop.
/// Generic over [`Pipe`] so tests can substitute a scripted transport — real
/// use is `Session<Transport>`, the default.
pub struct Session<P: Pipe = Transport> {
    transport: P,
    tx: u32,
}

/// Fill `buf` completely unless EOF comes first; a partially-filled bulk
/// chunk would end the USB data phase early via an unintended short packet.
fn read_fill(src: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match src.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

impl<P: Pipe> Session<P> {
    /// Open an MTP session, recovering from a stale one left behind by a
    /// previous client (`SessionAlreadyOpen` → close → reopen).
    pub fn open(transport: P) -> Result<Self> {
        let mut session = Self { transport, tx: 0 };
        match session.request(op::OPEN_SESSION, &[1]) {
            Ok(_) => Ok(session),
            Err(e)
                if e.to_string()
                    .contains(&response_name(ptp::RESP_SESSION_ALREADY_OPEN)) =>
            {
                // Stale session from a previous client: close and reopen.
                session.request(op::CLOSE_SESSION, &[]).ok();
                session.request(op::OPEN_SESSION, &[1])?;
                Ok(session)
            }
            Err(e) => Err(e),
        }
    }

    fn send_command(&mut self, code: u16, params: &[u32]) -> Result<u32> {
        self.tx += 1;
        let mut w = Writer::new();
        w.u32((12 + 4 * params.len()) as u32);
        w.u16(ptp::CONTAINER_COMMAND);
        w.u16(code);
        w.u32(self.tx);
        for p in params {
            w.u32(*p);
        }
        self.transport.write(w.into_inner())?;
        Ok(self.tx)
    }

    /// Read one complete container; returns (type, code, payload-after-header).
    fn read_container(&mut self) -> Result<(u16, u16, Vec<u8>)> {
        let mut buf = self.transport.read(CHUNK)?;
        if buf.len() < 12 {
            bail!("short PTP container ({} bytes)", buf.len());
        }
        let total = u32le(&buf[0..4]) as usize;
        let ctype = u16le(&buf[4..6]);
        let code = u16le(&buf[6..8]);
        while buf.len() < total {
            let more = self.transport.read(CHUNK)?;
            if more.is_empty() {
                bail!(
                    "device stalled mid-container ({}/{} bytes)",
                    buf.len(),
                    total
                );
            }
            buf.extend_from_slice(&more);
        }
        buf.drain(0..12);
        Ok((ctype, code, buf))
    }

    fn read_response(&mut self) -> Result<Vec<u32>> {
        let (ctype, code, payload) = self.read_container()?;
        if ctype != ptp::CONTAINER_RESPONSE {
            bail!("expected response container, got type {ctype}");
        }
        if code != ptp::RESP_OK {
            bail!("device returned {}", response_name(code));
        }
        Ok(payload.chunks_exact(4).map(u32le).collect())
    }

    /// Operation with no data phase.
    pub fn request(&mut self, code: u16, params: &[u32]) -> Result<Vec<u32>> {
        self.send_command(code, params)?;
        self.read_response()
    }

    /// Operation with a data-in phase; returns the data payload.
    pub fn request_read(&mut self, code: u16, params: &[u32]) -> Result<Vec<u8>> {
        self.send_command(code, params)?;
        let (ctype, ccode, payload) = self.read_container()?;
        if ctype == ptp::CONTAINER_RESPONSE {
            if ccode != ptp::RESP_OK {
                bail!("device returned {}", response_name(ccode));
            }
            return Ok(Vec::new()); // OK response with no data phase
        }
        self.read_response()?;
        Ok(payload)
    }

    /// Operation with a data-out phase.
    pub fn request_write(&mut self, code: u16, params: &[u32], data: &[u8]) -> Result<Vec<u32>> {
        let tx = self.send_command(code, params)?;
        let mut w = Writer::new();
        w.u32((12 + data.len()) as u32);
        w.u16(ptp::CONTAINER_DATA);
        w.u16(code);
        w.u32(tx);
        w.bytes(data);
        self.transport.write(w.into_inner())?;
        self.read_response()
    }

    // ---- high-level operations ------------------------------------------

    /// Manufacturer, model, firmware version, and serial of the device.
    pub fn device_info(&mut self) -> Result<DeviceInfo> {
        let data = self.request_read(op::GET_DEVICE_INFO, &[])?;
        DeviceInfo::parse(&data)
    }

    /// IDs of the device's storages (internal memory, SD card, …).
    pub fn storage_ids(&mut self) -> Result<Vec<u32>> {
        let data = self.request_read(op::GET_STORAGE_IDS, &[])?;
        Reader::new(&data).array_u32()
    }

    /// Capacity and naming of one storage.
    pub fn storage_info(&mut self, id: u32) -> Result<StorageInfo> {
        let data = self.request_read(op::GET_STORAGE_INFO, &[id])?;
        StorageInfo::parse(&data)
    }

    /// Full metadata of one object (file or folder).
    pub fn object_info(&mut self, handle: u32) -> Result<ObjectInfo> {
        let data = self.request_read(op::GET_OBJECT_INFO, &[handle])?;
        ObjectInfo::parse(&data)
    }

    /// List a directory, sorted folders-first then case-insensitively by
    /// name. Pass [`crate::ptp::PARENT_ROOT`] as `parent` for a storage root.
    ///
    /// One `GetObjectInfo` round-trip per entry — callers listing repeatedly
    /// should cache.
    pub fn list(&mut self, storage: u32, parent: u32) -> Result<Vec<Entry>> {
        let data = self.request_read(op::GET_OBJECT_HANDLES, &[storage, 0, parent])?;
        let handles = Reader::new(&data).array_u32()?;
        let mut entries = Vec::with_capacity(handles.len());
        for handle in handles {
            let info = self.object_info(handle)?;
            entries.push(Entry {
                handle,
                is_dir: info.is_folder(),
                size: info.compressed_size as u64,
                name: info.filename,
            });
        }
        entries.sort_by_key(|e| (!e.is_dir, e.name.to_lowercase()));
        Ok(entries)
    }

    /// Delete an object. For folders, most devices delete recursively.
    pub fn delete(&mut self, handle: u32) -> Result<()> {
        self.request(op::DELETE_OBJECT, &[handle, 0])?;
        Ok(())
    }

    /// Create a folder and return its new handle.
    pub fn make_folder(&mut self, storage: u32, parent: u32, name: &str) -> Result<u32> {
        let info = ObjectInfo {
            storage_id: storage,
            format: ptp::FMT_ASSOCIATION,
            compressed_size: 0,
            parent,
            association_type: 1, // generic folder
            filename: name.to_string(),
            modification_date: String::new(),
        };
        let resp =
            self.request_write(op::SEND_OBJECT_INFO, &[storage, parent], &info.serialize()?)?;
        match resp.get(2) {
            Some(&handle) => Ok(handle),
            None => bail!("device did not return a handle for the new folder"),
        }
    }

    /// Upload: SendObjectInfo announcing the file, then SendObject streaming
    /// it in [`CHUNK`]-sized pieces, so large files never sit in memory.
    ///
    /// `progress` receives (bytes sent, total) after each chunk. Returns the
    /// new object's handle.
    pub fn send_file(
        &mut self,
        storage: u32,
        parent: u32,
        name: &str,
        src: &mut impl Read,
        size: u64,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<u32> {
        let info = ObjectInfo {
            storage_id: storage,
            format: ptp::FMT_UNDEFINED,
            compressed_size: u32::try_from(size).unwrap_or(u32::MAX),
            parent,
            association_type: 0,
            filename: name.to_string(),
            modification_date: String::new(),
        };
        let resp =
            self.request_write(op::SEND_OBJECT_INFO, &[storage, parent], &info.serialize()?)?;
        let Some(&handle) = resp.get(2) else {
            bail!("device did not return a handle for {name:?}");
        };

        let tx = self.send_command(op::SEND_OBJECT, &[])?;
        let total = size + 12;
        let mut header = Writer::new();
        header.u32(u32::try_from(total).unwrap_or(u32::MAX));
        header.u16(ptp::CONTAINER_DATA);
        header.u16(op::SEND_OBJECT);
        header.u32(tx);

        let mut sent: u64 = 0;
        let mut chunk = header.into_inner();
        loop {
            chunk.resize(CHUNK, 0);
            let offset = if sent == 0 { 12 } else { 0 };
            let n = read_fill(src, &mut chunk[offset..])?;
            chunk.truncate(offset + n);
            if chunk.is_empty() {
                break;
            }
            let payload = (chunk.len() - offset) as u64;
            self.transport.write(std::mem::take(&mut chunk))?;
            sent += payload;
            progress(sent, size);
            if sent >= size {
                break;
            }
        }
        self.read_response()?;
        Ok(handle)
    }

    /// Download: GetObject, streaming the data phase to `dst` chunk by chunk.
    ///
    /// `progress` receives (bytes received, total); total is 0 when the
    /// device streams with unknown length.
    pub fn get_file(
        &mut self,
        handle: u32,
        dst: &mut impl std::io::Write,
        mut progress: impl FnMut(u64, u64),
    ) -> Result<()> {
        self.send_command(op::GET_OBJECT, &[handle])?;
        let first = self.transport.read(CHUNK)?;
        if first.len() < 12 {
            bail!("short PTP container ({} bytes)", first.len());
        }
        let total = u32le(&first[0..4]) as u64;
        let ctype = u16le(&first[4..6]);
        let code = u16le(&first[6..8]);
        if ctype == ptp::CONTAINER_RESPONSE {
            bail!("device returned {}", response_name(code));
        }
        // total == u32::MAX means "unknown length, ends on short packet".
        let unknown_len = total == u32::MAX as u64;
        let data_total = if unknown_len { 0 } else { total - 12 };

        dst.write_all(&first[12..])?;
        let mut received = (first.len() - 12) as u64;
        progress(received, data_total);
        loop {
            if !unknown_len && received >= data_total {
                break;
            }
            let chunk = self.transport.read(CHUNK)?;
            if chunk.is_empty() {
                if unknown_len {
                    break;
                }
                bail!("device stalled mid-transfer ({received}/{data_total} bytes)");
            }
            received += chunk.len() as u64;
            let short = chunk.len() < CHUNK;
            dst.write_all(&chunk)?;
            progress(received, data_total);
            if unknown_len && short {
                break;
            }
        }
        dst.flush()?;
        self.read_response()?;
        Ok(())
    }
}

impl<P: Pipe> Drop for Session<P> {
    fn drop(&mut self) {
        let _ = self.request(op::CLOSE_SESSION, &[]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// Scripted pipe: `read` pops pre-built containers, `write` records.
    struct MockPipe {
        reads: RefCell<VecDeque<Vec<u8>>>,
        writes: RefCell<Vec<Vec<u8>>>,
    }

    impl MockPipe {
        fn new(reads: Vec<Vec<u8>>) -> Self {
            Self {
                reads: RefCell::new(reads.into()),
                writes: RefCell::new(Vec::new()),
            }
        }
    }

    impl Pipe for MockPipe {
        fn write(&self, data: Vec<u8>) -> Result<()> {
            self.writes.borrow_mut().push(data);
            Ok(())
        }

        fn read(&self, _max: usize) -> Result<Vec<u8>> {
            match self.reads.borrow_mut().pop_front() {
                Some(chunk) => Ok(chunk),
                None => bail!("mock: no more scripted reads"),
            }
        }
    }

    fn container(ctype: u16, code: u16, payload: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32((12 + payload.len()) as u32);
        w.u16(ctype);
        w.u16(code);
        w.u32(0); // transaction id (not verified by the session)
        w.bytes(payload);
        w.into_inner()
    }

    fn resp(code: u16, params: &[u32]) -> Vec<u8> {
        let mut w = Writer::new();
        for p in params {
            w.u32(*p);
        }
        container(ptp::CONTAINER_RESPONSE, code, &w.into_inner())
    }

    fn ok(params: &[u32]) -> Vec<u8> {
        resp(ptp::RESP_OK, params)
    }

    fn data(code: u16, payload: &[u8]) -> Vec<u8> {
        container(ptp::CONTAINER_DATA, code, payload)
    }

    fn open_session(reads: Vec<Vec<u8>>) -> Session<MockPipe> {
        let mut script = vec![ok(&[])]; // OpenSession response
        script.extend(reads);
        Session::open(MockPipe::new(script)).unwrap()
    }

    fn object_info_bytes(name: &str, format: u16, size: u32) -> Vec<u8> {
        ObjectInfo {
            storage_id: 0x10001,
            format,
            compressed_size: size,
            parent: ptp::PARENT_ROOT,
            association_type: 0,
            filename: name.into(),
            modification_date: String::new(),
        }
        .serialize()
        .unwrap()
    }

    // ---- read_fill -------------------------------------------------------

    /// Reader yielding at most 3 bytes per call, to simulate short reads.
    struct TrickleReader(Vec<u8>);

    impl Read for TrickleReader {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = self.0.len().min(buf.len()).min(3);
            let tail = self.0.split_off(n);
            buf[..n].copy_from_slice(&self.0);
            self.0 = tail;
            Ok(n)
        }
    }

    #[test]
    fn read_fill_completes_despite_short_reads() {
        let mut src = TrickleReader((0..=99).collect());
        let mut buf = [0u8; 100];
        assert_eq!(read_fill(&mut src, &mut buf).unwrap(), 100);
        assert_eq!(buf[99], 99);
    }

    #[test]
    fn read_fill_stops_at_eof() {
        let mut src = TrickleReader(vec![1, 2, 3, 4, 5]);
        let mut buf = [0u8; 100];
        assert_eq!(read_fill(&mut src, &mut buf).unwrap(), 5);
        assert_eq!(&buf[..5], &[1, 2, 3, 4, 5]);
    }

    // ---- endian helpers --------------------------------------------------

    #[test]
    fn endian_helpers() {
        assert_eq!(u16le(&[0x34, 0x12]), 0x1234);
        assert_eq!(u32le(&[0x78, 0x56, 0x34, 0x12]), 0x12345678);
    }

    // ---- session open ----------------------------------------------------

    #[test]
    fn open_sends_open_session_command() {
        let session = open_session(vec![]);
        let writes = session.transport.writes.borrow();
        assert_eq!(writes.len(), 1);
        let cmd = &writes[0];
        assert_eq!(u16le(&cmd[4..6]), ptp::CONTAINER_COMMAND);
        assert_eq!(u16le(&cmd[6..8]), op::OPEN_SESSION);
        assert_eq!(u32le(&cmd[12..16]), 1); // session id
    }

    #[test]
    fn open_recovers_from_stale_session() {
        let pipe = MockPipe::new(vec![
            resp(ptp::RESP_SESSION_ALREADY_OPEN, &[]),
            ok(&[]), // CloseSession
            ok(&[]), // second OpenSession
        ]);
        let session = Session::open(pipe).unwrap();
        let codes: Vec<u16> = session
            .transport
            .writes
            .borrow()
            .iter()
            .map(|w| u16le(&w[6..8]))
            .collect();
        assert_eq!(
            codes,
            vec![op::OPEN_SESSION, op::CLOSE_SESSION, op::OPEN_SESSION]
        );
    }

    #[test]
    fn open_propagates_other_errors() {
        let Err(err) = Session::open(MockPipe::new(vec![resp(0x2002, &[])])) else {
            panic!("open should have failed");
        };
        assert!(err.to_string().contains("GeneralError"), "{err}");
    }

    // ---- transaction plumbing --------------------------------------------

    #[test]
    fn request_returns_response_params() {
        let mut session = open_session(vec![ok(&[7, 8, 9])]);
        assert_eq!(session.request(0x1001, &[]).unwrap(), vec![7, 8, 9]);
    }

    #[test]
    fn request_reports_error_responses_by_name() {
        let mut session = open_session(vec![resp(0x2009, &[])]);
        let err = session.request(op::GET_OBJECT_INFO, &[42]).unwrap_err();
        assert!(err.to_string().contains("InvalidObjectHandle"), "{err}");
    }

    #[test]
    fn request_rejects_short_containers() {
        let mut session = open_session(vec![vec![1, 2, 3]]);
        let err = session.request(0x1001, &[]).unwrap_err();
        assert!(err.to_string().contains("short PTP container"), "{err}");
    }

    #[test]
    fn read_container_reassembles_split_containers() {
        let full = data(op::GET_OBJECT, &[0xAA; 40]);
        let (a, b) = full.split_at(20);
        let mut session = open_session(vec![a.to_vec(), b.to_vec(), ok(&[])]);
        let payload = session.request_read(op::GET_OBJECT, &[1]).unwrap();
        assert_eq!(payload, vec![0xAA; 40]);
    }

    #[test]
    fn request_read_returns_payload_then_checks_response() {
        let mut session = open_session(vec![data(0x1004, &[1, 2, 3]), ok(&[])]);
        assert_eq!(session.request_read(0x1004, &[]).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn request_read_accepts_ok_without_data_phase() {
        let mut session = open_session(vec![ok(&[])]);
        assert_eq!(session.request_read(0x1004, &[]).unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn request_read_reports_error_without_data_phase() {
        let mut session = open_session(vec![resp(0x2008, &[])]);
        let err = session
            .request_read(op::GET_STORAGE_INFO, &[1])
            .unwrap_err();
        assert!(err.to_string().contains("InvalidStorageID"), "{err}");
    }

    #[test]
    fn request_write_frames_command_and_data() {
        let mut session = open_session(vec![ok(&[])]);
        session
            .request_write(op::SEND_OBJECT_INFO, &[1, 2], &[9, 9])
            .unwrap();
        let writes = session.transport.writes.borrow();
        let cmd = &writes[1];
        assert_eq!(u16le(&cmd[4..6]), ptp::CONTAINER_COMMAND);
        assert_eq!(u32le(&cmd[0..4]) as usize, 12 + 8); // two params
        let dat = &writes[2];
        assert_eq!(u16le(&dat[4..6]), ptp::CONTAINER_DATA);
        assert_eq!(u32le(&dat[0..4]) as usize, 12 + 2);
        assert_eq!(&dat[12..], &[9, 9]);
    }

    #[test]
    fn transaction_ids_increment() {
        let mut session = open_session(vec![ok(&[]), ok(&[])]);
        session.request(0x1001, &[]).unwrap();
        session.request(0x1001, &[]).unwrap();
        let writes = session.transport.writes.borrow();
        let txs: Vec<u32> = writes.iter().map(|w| u32le(&w[8..12])).collect();
        assert_eq!(txs, vec![1, 2, 3]);
    }

    // ---- high-level operations -------------------------------------------

    #[test]
    fn storage_ids_parses_the_id_array() {
        let mut w = Writer::new();
        w.u32(2);
        w.u32(0x10001);
        w.u32(0x20001);
        let mut session = open_session(vec![data(op::GET_STORAGE_IDS, &w.into_inner()), ok(&[])]);
        assert_eq!(session.storage_ids().unwrap(), vec![0x10001, 0x20001]);
    }

    #[test]
    fn storage_info_parses_the_dataset() {
        let mut w = Writer::new();
        w.u16(3); // storage type
        w.u16(2); // fs type
        w.u16(0); // access
        w.u64(64_000_000_000);
        w.u64(10_000_000_000);
        w.u32(0);
        w.string("Internal shared storage").unwrap();
        w.string("BOOX").unwrap();
        let mut session = open_session(vec![data(op::GET_STORAGE_INFO, &w.into_inner()), ok(&[])]);
        let info = session.storage_info(0x10001).unwrap();
        assert_eq!(info.max_capacity, 64_000_000_000);
        assert_eq!(info.free_space, 10_000_000_000);
        assert_eq!(info.description, "Internal shared storage");
        assert_eq!(info.volume_label, "BOOX");
    }

    #[test]
    fn list_fetches_infos_and_sorts_dirs_first() {
        let mut handles = Writer::new();
        handles.u32(2);
        handles.u32(5);
        handles.u32(6);
        let mut session = open_session(vec![
            data(op::GET_OBJECT_HANDLES, &handles.into_inner()),
            ok(&[]),
            data(
                op::GET_OBJECT_INFO,
                &object_info_bytes("zeta.txt", ptp::FMT_UNDEFINED, 42),
            ),
            ok(&[]),
            data(
                op::GET_OBJECT_INFO,
                &object_info_bytes("Books", ptp::FMT_ASSOCIATION, 0),
            ),
            ok(&[]),
        ]);
        let entries = session.list(0x10001, ptp::PARENT_ROOT).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "Books");
        assert!(entries[0].is_dir);
        assert_eq!(entries[1].name, "zeta.txt");
        assert_eq!(entries[1].size, 42);
        assert_eq!(entries[1].handle, 5);
    }

    #[test]
    fn delete_sends_handle_and_zero() {
        let mut session = open_session(vec![ok(&[])]);
        session.delete(77).unwrap();
        let writes = session.transport.writes.borrow();
        let cmd = writes.last().unwrap();
        assert_eq!(u16le(&cmd[6..8]), op::DELETE_OBJECT);
        assert_eq!(u32le(&cmd[12..16]), 77);
        assert_eq!(u32le(&cmd[16..20]), 0);
    }

    #[test]
    fn make_folder_returns_new_handle() {
        let mut session = open_session(vec![ok(&[0x10001, 0xFFFF_FFFF, 123])]);
        assert_eq!(
            session
                .make_folder(0x10001, ptp::PARENT_ROOT, "New")
                .unwrap(),
            123
        );
        let writes = session.transport.writes.borrow();
        let dataset = ObjectInfo::parse(&writes.last().unwrap()[12..]).unwrap();
        assert_eq!(dataset.format, ptp::FMT_ASSOCIATION);
        assert_eq!(dataset.filename, "New");
    }

    #[test]
    fn make_folder_requires_a_handle_in_the_response() {
        let mut session = open_session(vec![ok(&[])]);
        let err = session
            .make_folder(0x10001, ptp::PARENT_ROOT, "New")
            .unwrap_err();
        assert!(err.to_string().contains("handle"), "{err}");
    }

    #[test]
    fn send_file_streams_content_after_object_info() {
        let content = b"hello device";
        let mut session = open_session(vec![
            ok(&[0x10001, ptp::PARENT_ROOT, 55]), // SendObjectInfo response
            ok(&[]),                              // SendObject response
        ]);
        let mut progress_calls = 0;
        let handle = session
            .send_file(
                0x10001,
                ptp::PARENT_ROOT,
                "hello.txt",
                &mut &content[..],
                content.len() as u64,
                |_, _| progress_calls += 1,
            )
            .unwrap();
        assert_eq!(handle, 55);
        assert!(progress_calls > 0);
        let writes = session.transport.writes.borrow();
        // open, SendObjectInfo cmd, SendObjectInfo data, SendObject cmd, data
        assert_eq!(writes.len(), 5);
        let info = ObjectInfo::parse(&writes[2][12..]).unwrap();
        assert_eq!(info.filename, "hello.txt");
        assert_eq!(info.compressed_size, content.len() as u32);
        let dat = &writes[4];
        assert_eq!(u32le(&dat[0..4]) as usize, 12 + content.len());
        assert_eq!(u16le(&dat[4..6]), ptp::CONTAINER_DATA);
        assert_eq!(&dat[12..], content);
    }

    #[test]
    fn send_file_handles_empty_files() {
        let mut session = open_session(vec![ok(&[0x10001, ptp::PARENT_ROOT, 9]), ok(&[])]);
        session
            .send_file(
                0x10001,
                ptp::PARENT_ROOT,
                "empty",
                &mut std::io::empty(),
                0,
                |_, _| {},
            )
            .unwrap();
        let writes = session.transport.writes.borrow();
        let dat = writes.last().unwrap();
        assert_eq!(dat.len(), 12); // header-only data phase
        assert_eq!(u32le(&dat[0..4]), 12);
    }

    #[test]
    fn get_file_streams_known_length_payload() {
        let payload = vec![0x5A; 100];
        let mut session = open_session(vec![data(op::GET_OBJECT, &payload), ok(&[])]);
        let mut out = Vec::new();
        session.get_file(31, &mut out, |_, _| {}).unwrap();
        assert_eq!(out, payload);
        let writes = session.transport.writes.borrow();
        let cmd = writes.last().unwrap();
        assert_eq!(u16le(&cmd[6..8]), op::GET_OBJECT);
        assert_eq!(u32le(&cmd[12..16]), 31);
    }

    #[test]
    fn get_file_handles_multi_chunk_payloads() {
        let full = data(op::GET_OBJECT, &[7u8; 50]);
        let (a, b) = full.split_at(30);
        let mut session = open_session(vec![a.to_vec(), b.to_vec(), ok(&[])]);
        let mut out = Vec::new();
        session.get_file(1, &mut out, |_, _| {}).unwrap();
        assert_eq!(out, vec![7u8; 50]);
    }

    #[test]
    fn get_file_reports_device_errors() {
        let mut session = open_session(vec![resp(0x2009, &[])]);
        let mut out = Vec::new();
        let err = session.get_file(999, &mut out, |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("InvalidObjectHandle"), "{err}");
        assert!(out.is_empty());
    }

    #[test]
    fn get_file_handles_unknown_length_until_short_read() {
        // Container claims unknown length (0xFFFFFFFF): data ends on short read.
        let mut first = Writer::new();
        first.u32(u32::MAX);
        first.u16(ptp::CONTAINER_DATA);
        first.u16(op::GET_OBJECT);
        first.u32(0);
        first.bytes(&[1, 2, 3]);
        let mut session = open_session(vec![first.into_inner(), vec![4, 5], ok(&[])]);
        let mut out = Vec::new();
        session.get_file(1, &mut out, |_, _| {}).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5]);
    }
}
