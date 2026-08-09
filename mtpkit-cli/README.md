# mtpkit-cli

Command-line MTP client built on the [`mtpkit`](../mtpkit/) library. Installs a single binary, `mtpkit`.

## Usage

Run `mtpkit` with no arguments for an interactive shell, or pass each command as one quoted argument for scripting:

```
mtpkit
mtpkit 'cd Books' 'put ~/Downloads/novel.epub'
mtpkit 'get Documents/notes.pdf ~/notes.pdf'
```

## Commands

| Command | Effect |
|---|---|
| `ls [path]` | list current directory (or path); refreshes the listing cache |
| `cd <path>` | change directory — `/` is the storage list, `..` goes up, paths may span segments |
| `pwd` | print current location |
| `put <file> [dir]` | upload a local file, optionally into a remote directory |
| `get <name> [dst]` | download a remote file, optionally to a local path or directory |
| `mkdir <name>` | create a folder |
| `rm <name>` | delete a file or folder (folders may delete recursively — device-dependent) |
| `storages` | list storages with free/total capacity |
| `info` | show device manufacturer, model, firmware, serial |
| `help`, `quit` | the obvious |

With a single storage the shell starts inside it; with several you start at the storage list and `cd` into one.

## Tab-completion

In the interactive shell, Tab completes command names, remote paths, and local paths, with quote and backslash escaping handled both ways (`My Books` completes to `My\ Books`):

- `cd`, `mkdir`, and `put`'s destination complete remote folders only.
- `ls`, `rm`, and `get` complete remote files and folders; folders complete with a trailing `/` so you can keep drilling down.
- `put`'s first argument and `get`'s destination complete local filesystem paths.

Completions are served from a per-directory cache shared with navigation: the first visit to a directory queries the device, later Tabs are instant. An explicit `ls` refreshes the current directory; `put`/`mkdir`/`rm` invalidate the cache.

## Troubleshooting

`MTPKIT_DEBUG=1 mtpkit` prints every USB device and interface considered during detection — the first thing to check when the device isn't found. Common causes: the device isn't on the bus (charge-only cable or dock port), its screen is locked, or another program has the MTP interface claimed.

## File layout

| File | Role |
|---|---|
| `main.rs` | entry point: device discovery, one-shot vs interactive dispatch, the readline loop |
| `shell.rs` | the `Shell`: command execution, remote-path navigation, the listing cache, pure helpers (tokenizer, path splitting, size formatting) |
| `complete.rs` | the rustyline helper: quote-aware word scanning and command/remote/local completion |

`Shell` is generic over the library's `Pipe` trait, so navigation and caching are unit-tested against a scripted device — see the tests at the bottom of `shell.rs`.
