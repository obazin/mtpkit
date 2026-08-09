# mtpkit (library)

Pure-Rust client implementation of MTP (Media Transfer Protocol) over USB. It lets an application enumerate a device's storages, browse folders, and stream files in both directions — without libusb, kernel drivers, or FUSE.

## Architecture

The crate is three layers with a strict downward dependency order — the session executes transactions, the wire format encodes them, the transport moves the bytes:

```
your application (CLI, GUI, …)
      │  Session::{list, send_file, get_file, make_folder, delete, …}
      ▼
session.rs    MTP transactions: command → [data phase] → response
      │  Pipe trait: write(Vec<u8>) / read(max)
      ▼
usb.rs        device discovery + bulk USB transport (nusb: IOKit on macOS, usbfs on Linux)

ptp.rs        wire format used by session.rs: containers, datasets, Reader/Writer
```

The `Pipe` trait between the session and the transport is the crate's testing seam: `Session` is generic over it (defaulting to the real USB `Transport`), so the whole protocol layer is unit-tested against a scripted mock transport — no hardware needed.

## File by file

### `lib.rs`

Crate root: module wiring and the crate-level documentation, including a usage example.

### `ptp.rs`

The PTP/MTP wire format, everything little-endian per the spec:

- Container constants (command / data / response) and the operation codes mtpkit uses (`OpenSession`, `GetObjectHandles`, `SendObject`, …).
- `response_name` — human-readable names for device response codes, so errors read `InvalidObjectHandle (0x2009)` instead of a bare number.
- `Reader` / `Writer` — cursor-style decoder and buffer builder for the primitive types PTP datasets are made of: `u8`–`u64`, UTF-16 length-prefixed strings, and counted arrays.
- The three datasets mtpkit needs: `ObjectInfo` (file/folder metadata, both parsed and serialized — uploads announce themselves with it), `StorageInfo`, and `DeviceInfo`.

### `usb.rs`

USB device discovery and the bulk transport:

- `list_devices` — enumerates USB devices and keeps those exposing an MTP interface. Detection accepts two shapes learned from real hardware: the standard still-image class (6/1, any protocol byte), or a vendor-specific interface whose string descriptor is named "MTP" — the shape Android uses for its always-on MTP. Set `MTPKIT_DEBUG=1` to dump every device and interface considered.
- `Transport` — claims the MTP interface and exposes blocking bulk `write`/`read` over its endpoints.
- `CHUNK` (1 MiB) — the transfer granularity. It is a multiple of every USB max packet size, which matters: an intermediate chunk that ended on a partial packet would be read by the device as a premature end of the data phase.

### `session.rs`

The protocol brain, in two halves:

- Transaction plumbing: `request` (no data phase), `request_read` (data in), `request_write` (data out) frame commands, reassemble containers that arrive split across bulk reads, and turn non-OK responses into named errors. Transaction IDs are sequenced per session.
- High-level operations: storage enumeration, directory listing (sorted folders-first), `make_folder`, `delete`, and streaming transfers — `send_file` and `get_file` move data in `CHUNK` pieces so multi-gigabyte files never sit in memory, report progress via callback, and handle the edge cases (empty files, unknown-length transfers that end on a short packet, devices that answer an error instead of a data phase).

`Session::open` also recovers from a stale session left behind by a previous client (`SessionAlreadyOpen` → close → reopen), and `Drop` closes the session politely.

## Example

```rust
use mtpkit::{session::Session, usb};

let devices = usb::list_devices()?;
let transport = usb::Transport::open(&devices[0])?;
let mut session = Session::open(transport)?;

for id in session.storage_ids()? {
    let info = session.storage_info(id)?;
    println!("{}: {} bytes free", info.description, info.free_space);
}
```

## Testing

`cargo test` — every layer that doesn't require hardware is covered: dataset round-trips and error paths in `ptp`, and the full transaction/streaming logic in `session` driven through a mock `Pipe` with scripted device responses. Only `usb.rs` (thin glue over nusb) requires a real device.
