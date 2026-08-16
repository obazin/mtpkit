# mtpkit

A pure-Rust MTP (Media Transfer Protocol) toolkit for talking to Android devices — phones, tablets, e-readers — over USB. No libusb, no kernel driver, no FUSE: USB access goes through [nusb](https://crates.io/crates/nusb), which speaks IOKit directly on macOS and usbfs on Linux, and the MTP/PTP protocol is implemented from scratch in the `mtpkit` library.

Born as a Rust port of the CLI subset of [android-file-transfer-linux](https://github.com/whoozle/android-file-transfer-linux), and developed against an ONYX BOOX e-ink tablet.

## Workspace layout

| Crate | What it is |
|---|---|
| [`mtpkit`](mtpkit/) | The library: USB discovery and transport, PTP/MTP wire format, session and file operations |
| [`mtpkit-cli`](mtpkit-cli/) | The command-line client (binary `mtpkit`): interactive shell with tab-completion, one-shot commands |
| [`mtpkit-gui`](mtpkit-gui/) | Placeholder for a future graphical client |

## Quick start

With Nix (the flake pins the whole toolchain — pair it with direnv and `use flake` for automatic activation):

```
nix build          # → result/bin/mtpkit
nix develop        # dev shell: pinned toolchain, cargo tools, LSPs, formatters
nix flake check    # fmt + clippy + the test suite, on that same toolchain
```

The pin lives in [`obazin/rust-projects`](https://github.com/obazin/rust-projects), the shared flake this one consumes: the compiler is a fixed-output derivation there, so `nix flake update` cannot move it and bumping Rust is a single commit for every project at once.

Plain cargo works too, with any Rust toolchain supporting edition 2024 (1.85+):

```
cargo build --release        # → target/release/mtpkit
cargo test --workspace       # 70 tests, no device needed
```

Then plug in the device, unlock its screen, and:

```
mtpkit                                        # interactive shell
mtpkit 'cd Books' 'put ~/Downloads/novel.epub'   # one-shot commands
```

## Platform support

macOS (Apple Silicon and Intel) and Linux, via nusb. The USB transport claims the device's MTP interface directly, so no other program (mounted MTP volume, file manager) may be using the device at the same time.

## License

LGPL-2.1-or-later, matching the project that inspired it.
