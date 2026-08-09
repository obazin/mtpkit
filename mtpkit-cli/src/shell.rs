//! The interactive shell: command execution, remote-path navigation, and a
//! per-directory listing cache shared with tab-completion.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, IsTerminal as _, Write as _};
use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};

use mtpkit::ptp::PARENT_ROOT;
use mtpkit::session::{Entry, Pipe, Session};
use mtpkit::usb::Transport;

/// Help text: printed by `--help`, the `help` command, and on bad commands.
pub const USAGE: &str = "\
usage: mtpkit [command]...

Runs each quoted argument as one command, or starts an interactive shell.
In the shell, Tab completes commands, remote paths, and local files.

commands:
  ls [path]            list current directory (or path)
  cd <path>            change directory ('/' lists storages, '..' goes up)
  pwd                  print current directory
  put <file> [dir]     upload local file (into dir if given)
  get <name> [dst]     download file (to dst if given)
  mkdir <name>         create folder
  rm <name>            delete file or folder
  storages             list storages with capacity
  info                 show device information
  help                 show this help
  quit                 leave the interactive shell

examples:
  mtpkit
  mtpkit 'cd Books' 'put ~/Downloads/novel.epub'
  mtpkit 'get Documents/notes.pdf ~/notes.pdf'";

/// A snapshot of the shell's location: (storage index, folder stack).
type SavedLocation = (Option<usize>, Vec<(u32, String)>);

/// The interactive shell: an MTP [`Session`] plus a current location, the
/// listing cache, and the command interpreter.
///
/// Generic over [`Pipe`] like the session itself, so navigation and caching
/// are unit-tested against a scripted device (see tests below).
pub struct Shell<P: Pipe = Transport> {
    session: Session<P>,
    storages: Vec<(u32, String)>,
    storage: Option<usize>,
    dirs: Vec<(u32, String)>,
    /// Directory listings by (storage, parent handle). Cleared on any
    /// mutation; refreshed by an explicit `ls`.
    cache: HashMap<(u32, u32), Vec<Entry>>,
}

impl<P: Pipe> Shell<P> {
    /// Discover the device's storages and pick the starting location: inside
    /// the storage when there is exactly one, at the storage list otherwise.
    pub fn new(mut session: Session<P>) -> Result<Self> {
        let ids = session.storage_ids()?;
        let mut storages = Vec::new();
        for id in ids {
            let name = match session.storage_info(id) {
                Ok(info) if !info.volume_label.is_empty() => info.volume_label,
                Ok(info) if !info.description.is_empty() => info.description,
                _ => format!("storage-{id:08x}"),
            };
            storages.push((id, name.replace('/', "_")));
        }
        if storages.is_empty() {
            bail!("device reports no storage — unlock the device screen and try again");
        }
        let mut shell = Self {
            session,
            storages,
            storage: None,
            dirs: Vec::new(),
            cache: HashMap::new(),
        };
        // Single storage: start inside it rather than at the storage list.
        if shell.storages.len() == 1 {
            shell.storage = Some(0);
        }
        Ok(shell)
    }

    /// The current location as a `/`-joined path, `"/"` at the storage list.
    pub fn pwd(&self) -> String {
        let mut parts = vec![String::new()];
        if let Some(si) = self.storage {
            parts.push(self.storages[si].1.clone());
            parts.extend(self.dirs.iter().map(|(_, name)| name.clone()));
        }
        if parts.len() == 1 {
            "/".to_string()
        } else {
            parts.join("/")
        }
    }

    /// Tokenize and run one command line. Returns `Ok(false)` when the user
    /// asked to quit, `Ok(true)` to keep going.
    pub fn exec_line(&mut self, line: &str) -> Result<bool> {
        let tokens = tokenize(line)?;
        let Some((cmd, args)) = tokens.split_first() else {
            return Ok(true);
        };
        let arg = |i: usize| -> Result<&str> {
            args.get(i)
                .map(String::as_str)
                .with_context(|| format!("{cmd}: missing argument (see 'help')"))
        };
        match cmd.as_str() {
            "help" | "?" => println!("{USAGE}"),
            "quit" | "exit" | "q" => return Ok(false),
            "pwd" => println!("{}", self.pwd()),
            "info" => {
                let info = self.session.device_info()?;
                println!(
                    "{} {} (firmware {}, serial {})",
                    info.manufacturer, info.model, info.device_version, info.serial
                );
            }
            "storages" | "df" => {
                for (id, name) in self.storages.clone() {
                    match self.session.storage_info(id) {
                        Ok(info) => println!(
                            "{name:<24} {} free of {}",
                            human(info.free_space),
                            human(info.max_capacity)
                        ),
                        Err(e) => println!("{name:<24} (unavailable: {e:#})"),
                    }
                }
            }
            "ls" | "dir" => {
                let saved = self.save();
                let result = (|| {
                    if let Some(path) = args.first() {
                        self.change_dir(path)?;
                    }
                    // Explicit ls refreshes this directory's cache entry.
                    if let Ok(key) = self.target() {
                        self.cache.remove(&key);
                    }
                    self.list_current()
                })();
                self.restore(saved);
                for entry in result? {
                    if entry.is_dir {
                        println!("{:>10}  {}/", "dir", entry.name);
                    } else {
                        println!("{:>10}  {}", human(entry.size), entry.name);
                    }
                }
            }
            "cd" => self.change_dir(arg(0)?)?,
            "mkdir" => {
                let (dir, name) = split_remote_path(arg(0)?);
                let saved = self.save();
                let result = (|| {
                    if let Some(dir) = dir {
                        self.change_dir(dir)?;
                    }
                    let (storage, parent) = self.target()?;
                    self.session.make_folder(storage, parent, name)
                })();
                self.restore(saved);
                result?;
                self.cache.clear();
            }
            "rm" => {
                let (dir, name) = split_remote_path(arg(0)?);
                let saved = self.save();
                let result = (|| {
                    if let Some(dir) = dir {
                        self.change_dir(dir)?;
                    }
                    let (storage, parent) = self.target()?;
                    let entry = self.find(storage, parent, name)?;
                    self.session.delete(entry.handle)
                })();
                self.restore(saved);
                result?;
                self.cache.clear();
            }
            "put" => {
                let local = expand_local(arg(0)?);
                let name = local
                    .file_name()
                    .with_context(|| format!("{}: not a file path", local.display()))?
                    .to_string_lossy()
                    .into_owned();
                let file =
                    File::open(&local).with_context(|| format!("opening {}", local.display()))?;
                let size = file.metadata()?.len();
                let saved = self.save();
                let result = (|| {
                    if let Some(dir) = args.get(1) {
                        self.change_dir(dir)?;
                    }
                    let (storage, parent) = self.target()?;
                    let mut reader = BufReader::new(file);
                    self.session.send_file(
                        storage,
                        parent,
                        &name,
                        &mut reader,
                        size,
                        progress_bar(&name),
                    )
                })();
                self.restore(saved);
                result?;
                self.cache.clear();
                finish_progress();
            }
            "get" => {
                let (dir, name) = split_remote_path(arg(0)?);
                let saved = self.save();
                let result = (|| {
                    if let Some(dir) = dir {
                        self.change_dir(dir)?;
                    }
                    let (storage, parent) = self.target()?;
                    self.find(storage, parent, name)
                })();
                self.restore(saved);
                let entry = result?;
                if entry.is_dir {
                    bail!("{name} is a directory");
                }
                let mut dst = args
                    .get(1)
                    .map(|d| expand_local(d))
                    .unwrap_or_else(|| PathBuf::from(name));
                if dst.is_dir() {
                    dst.push(name);
                }
                let file =
                    File::create(&dst).with_context(|| format!("creating {}", dst.display()))?;
                let mut writer = BufWriter::new(file);
                self.session
                    .get_file(entry.handle, &mut writer, progress_bar(name))?;
                finish_progress();
            }
            other => bail!("unknown command {other:?} (see 'help')"),
        }
        Ok(true)
    }

    // ---- navigation ------------------------------------------------------

    /// Snapshot the current location, for commands (and completion) that
    /// navigate temporarily and must land back where they started.
    pub fn save(&self) -> SavedLocation {
        (self.storage, self.dirs.clone())
    }

    /// Return to a location captured by [`Shell::save`].
    pub fn restore(&mut self, saved: SavedLocation) {
        (self.storage, self.dirs) = saved;
    }

    /// Current (storage id, parent handle); errors at the storage-list root.
    fn target(&mut self) -> Result<(u32, u32)> {
        let Some(si) = self.storage else {
            bail!("at the storage list — 'cd' into a storage first");
        };
        let parent = self.dirs.last().map(|(h, _)| *h).unwrap_or(PARENT_ROOT);
        Ok((self.storages[si].0, parent))
    }

    /// Navigate a possibly multi-segment path: absolute (`/…`) resets to the
    /// root, `..` goes up, and each remaining segment must name a storage
    /// (at the root) or a folder (matched case-insensitively as a fallback).
    pub fn change_dir(&mut self, path: &str) -> Result<()> {
        if path.starts_with('/') {
            self.storage = if self.storages.len() == 1 {
                Some(0)
            } else {
                None
            };
            self.dirs.clear();
        }
        for comp in path.split('/').filter(|c| !c.is_empty() && *c != ".") {
            if comp == ".." {
                if self.dirs.pop().is_none() && self.storages.len() > 1 {
                    self.storage = None;
                }
                continue;
            }
            match self.storage {
                None => {
                    let idx = self
                        .storages
                        .iter()
                        .position(|(_, n)| n == comp)
                        .or_else(|| {
                            self.storages
                                .iter()
                                .position(|(_, n)| n.eq_ignore_ascii_case(comp))
                        })
                        .with_context(|| format!("no such storage: {comp}"))?;
                    self.storage = Some(idx);
                }
                Some(_) => {
                    let (storage, parent) = self.target()?;
                    let entry = self.find(storage, parent, comp)?;
                    if !entry.is_dir {
                        bail!("{comp} is not a directory");
                    }
                    self.dirs.push((entry.handle, entry.name));
                }
            }
        }
        Ok(())
    }

    fn find(&mut self, storage: u32, parent: u32, name: &str) -> Result<Entry> {
        let mut entries = self.list_dir(storage, parent)?;
        let idx = entries
            .iter()
            .position(|e| e.name == name)
            .or_else(|| {
                entries
                    .iter()
                    .position(|e| e.name.eq_ignore_ascii_case(name))
            })
            .with_context(|| format!("not found: {name}"))?;
        Ok(entries.swap_remove(idx))
    }

    /// Cached directory listing; hits the device only on the first request.
    fn list_dir(&mut self, storage: u32, parent: u32) -> Result<Vec<Entry>> {
        if let Some(cached) = self.cache.get(&(storage, parent)) {
            return Ok(cached.clone());
        }
        let entries = self.session.list(storage, parent)?;
        self.cache.insert((storage, parent), entries.clone());
        Ok(entries)
    }

    fn list_current(&mut self) -> Result<Vec<Entry>> {
        match self.storage {
            None => Ok(self
                .storages
                .iter()
                .map(|(_, name)| Entry {
                    handle: 0,
                    name: name.clone(),
                    is_dir: true,
                    size: 0,
                })
                .collect()),
            Some(_) => {
                let (storage, parent) = self.target()?;
                self.list_dir(storage, parent)
            }
        }
    }

    /// (name, is_dir) pairs at the current location, for tab-completion.
    pub fn completion_candidates(&mut self) -> Result<Vec<(String, bool)>> {
        Ok(self
            .list_current()?
            .into_iter()
            .map(|e| (e.name, e.is_dir))
            .collect())
    }
}

// ---- helpers -------------------------------------------------------------

/// Split "a/b/name" into (Some("a/b"), "name"); plain names have no dir part.
pub fn split_remote_path(path: &str) -> (Option<&str>, &str) {
    let path = path.trim_end_matches('/');
    match path.rsplit_once('/') {
        Some(("", name)) => (Some("/"), name),
        Some((dir, name)) => (Some(dir), name),
        None => (None, path),
    }
}

/// Expand a leading `~/` to the home directory (needed in the interactive
/// shell, where no sh runs before us).
pub fn expand_local(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

/// Format a byte count for humans: `0 B`, `1023 B`, `1.5 KB`, `3.0 GB`, …
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

fn progress_bar(name: &str) -> impl FnMut(u64, u64) + use<> {
    let name = name.to_string();
    let stderr_tty = std::io::stderr().is_terminal();
    move |done, total| {
        if !stderr_tty {
            return;
        }
        match (done * 100).checked_div(total) {
            Some(percent) => {
                eprint!(
                    "\r{name}: {percent:>3}% ({} / {})   ",
                    human(done),
                    human(total)
                );
            }
            None => eprint!("\r{name}: {}   ", human(done)),
        }
        let _ = std::io::stderr().flush();
    }
}

fn finish_progress() {
    if std::io::stderr().is_terminal() {
        eprintln!();
    }
}

/// Split a command line into words, honoring single/double quotes and
/// backslash escapes; errors on an unterminated quote.
pub fn tokenize(line: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut pending = false;
    let mut quote: Option<char> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => current.push(c),
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    pending = true;
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                        pending = true;
                    }
                }
                c if c.is_whitespace() => {
                    if pending {
                        tokens.push(std::mem::take(&mut current));
                        pending = false;
                    }
                }
                c => {
                    current.push(c);
                    pending = true;
                }
            },
        }
    }
    if quote.is_some() {
        bail!("unterminated quote");
    }
    if pending {
        tokens.push(current);
    }
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use mtpkit::ptp::{self, ObjectInfo, Writer, op};
    use std::cell::RefCell;
    use std::collections::VecDeque;

    // ---- pure helpers ----------------------------------------------------

    #[test]
    fn tokenize_splits_on_whitespace() {
        assert_eq!(tokenize("ls  Books  x").unwrap(), vec!["ls", "Books", "x"]);
        assert_eq!(tokenize("").unwrap(), Vec::<String>::new());
        assert_eq!(tokenize("   ").unwrap(), Vec::<String>::new());
    }

    #[test]
    fn tokenize_honors_quotes_and_escapes() {
        assert_eq!(
            tokenize(r#"put "My Book.epub" 'Two Words'"#).unwrap(),
            vec!["put", "My Book.epub", "Two Words"]
        );
        assert_eq!(tokenize(r"cd My\ Books").unwrap(), vec!["cd", "My Books"]);
        assert_eq!(tokenize(r#"echo """#).unwrap(), vec!["echo", ""]);
    }

    #[test]
    fn tokenize_rejects_unterminated_quotes() {
        assert!(tokenize("cd 'Books").is_err());
        assert!(tokenize("cd \"Books").is_err());
    }

    #[test]
    fn split_remote_path_cases() {
        assert_eq!(split_remote_path("name"), (None, "name"));
        assert_eq!(split_remote_path("a/b/name"), (Some("a/b"), "name"));
        assert_eq!(split_remote_path("/name"), (Some("/"), "name"));
        assert_eq!(split_remote_path("a/name/"), (Some("a"), "name"));
    }

    #[test]
    fn expand_local_expands_tilde() {
        if let Ok(home) = std::env::var("HOME") {
            assert_eq!(expand_local("~/x.txt"), PathBuf::from(home).join("x.txt"));
        }
        assert_eq!(expand_local("/tmp/x"), PathBuf::from("/tmp/x"));
        assert_eq!(expand_local("rel.txt"), PathBuf::from("rel.txt"));
    }

    #[test]
    fn human_formats_sizes() {
        assert_eq!(human(0), "0 B");
        assert_eq!(human(1023), "1023 B");
        assert_eq!(human(1024), "1.0 KB");
        assert_eq!(human(1536), "1.5 KB");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    // ---- Shell against a scripted session --------------------------------

    struct MockPipe {
        reads: RefCell<VecDeque<Vec<u8>>>,
    }

    impl Pipe for MockPipe {
        fn write(&self, _data: Vec<u8>) -> Result<()> {
            Ok(())
        }

        fn read(&self, _max: usize) -> Result<Vec<u8>> {
            match self.reads.borrow_mut().pop_front() {
                Some(chunk) => Ok(chunk),
                None => anyhow::bail!("mock: no more scripted reads"),
            }
        }
    }

    fn container(ctype: u16, code: u16, payload: &[u8]) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32((12 + payload.len()) as u32);
        w.u16(ctype);
        w.u16(code);
        w.u32(0);
        w.bytes(payload);
        w.into_inner()
    }

    fn ok() -> Vec<u8> {
        container(ptp::CONTAINER_RESPONSE, ptp::RESP_OK, &[])
    }

    fn data(code: u16, payload: &[u8]) -> Vec<u8> {
        container(ptp::CONTAINER_DATA, code, payload)
    }

    fn storage_info(label: &str) -> Vec<u8> {
        let mut w = Writer::new();
        w.u16(3);
        w.u16(2);
        w.u16(0);
        w.u64(1000);
        w.u64(500);
        w.u32(0);
        w.string("description").unwrap();
        w.string(label).unwrap();
        w.into_inner()
    }

    fn object_info(name: &str, is_dir: bool) -> Vec<u8> {
        ObjectInfo {
            storage_id: 0x10001,
            format: if is_dir {
                ptp::FMT_ASSOCIATION
            } else {
                ptp::FMT_UNDEFINED
            },
            compressed_size: 10,
            parent: ptp::PARENT_ROOT,
            association_type: 0,
            filename: name.into(),
            modification_date: String::new(),
        }
        .serialize()
        .unwrap()
    }

    fn handles(ids: &[u32]) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(ids.len() as u32);
        for id in ids {
            w.u32(*id);
        }
        w.into_inner()
    }

    /// Shell over a scripted session: OpenSession + storage discovery for
    /// `labels`, then the provided extra reads.
    fn shell_with(labels: &[&str], extra: Vec<Vec<u8>>) -> Shell<MockPipe> {
        let mut script = vec![ok()]; // OpenSession
        let mut ids = Writer::new();
        ids.u32(labels.len() as u32);
        for i in 0..labels.len() {
            ids.u32(0x10001 + i as u32 * 0x10000);
        }
        script.push(data(op::GET_STORAGE_IDS, &ids.into_inner()));
        script.push(ok());
        for label in labels {
            script.push(data(op::GET_STORAGE_INFO, &storage_info(label)));
            script.push(ok());
        }
        script.extend(extra);
        let session = Session::open(MockPipe {
            reads: RefCell::new(script.into()),
        })
        .unwrap();
        Shell::new(session).unwrap()
    }

    /// Scripted listing of one directory: handles + one object info each.
    fn listing(entries: &[(&str, bool)]) -> Vec<Vec<u8>> {
        let ids: Vec<u32> = (1..=entries.len() as u32).collect();
        let mut script = vec![data(op::GET_OBJECT_HANDLES, &handles(&ids)), ok()];
        for (name, is_dir) in entries {
            script.push(data(op::GET_OBJECT_INFO, &object_info(name, *is_dir)));
            script.push(ok());
        }
        script
    }

    #[test]
    fn single_storage_starts_inside_it() {
        let shell = shell_with(&["Tablet"], vec![]);
        assert_eq!(shell.pwd(), "/Tablet");
    }

    #[test]
    fn multiple_storages_start_at_the_storage_list() {
        let mut shell = shell_with(&["Internal", "SDCard"], vec![]);
        assert_eq!(shell.pwd(), "/");
        let names: Vec<String> = shell
            .completion_candidates()
            .unwrap()
            .into_iter()
            .map(|(n, d)| {
                assert!(d);
                n
            })
            .collect();
        assert_eq!(names, vec!["Internal", "SDCard"]);
    }

    #[test]
    fn cd_into_storage_and_folder() {
        let mut shell = shell_with(
            &["Internal", "SDCard"],
            listing(&[("Books", true), ("readme.txt", false)]),
        );
        shell.change_dir("Internal/Books").unwrap();
        assert_eq!(shell.pwd(), "/Internal/Books");
        shell.change_dir("..").unwrap();
        assert_eq!(shell.pwd(), "/Internal");
        shell.change_dir("..").unwrap();
        assert_eq!(shell.pwd(), "/");
    }

    #[test]
    fn listings_are_cached_across_navigation() {
        // Only ONE listing is scripted; the second cd must come from cache.
        let mut shell = shell_with(&["Tablet"], listing(&[("Books", true)]));
        shell.change_dir("Books").unwrap();
        shell.change_dir("..").unwrap();
        shell.change_dir("Books").unwrap(); // would fail if it re-queried
        assert_eq!(shell.pwd(), "/Tablet/Books");
    }

    #[test]
    fn cd_rejects_files_and_unknown_names() {
        let mut shell = shell_with(&["Tablet"], listing(&[("readme.txt", false)]));
        let err = shell.change_dir("readme.txt").unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
        let err = shell.change_dir("Nope").unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[test]
    fn cd_matches_names_case_insensitively() {
        let mut shell = shell_with(&["Tablet"], listing(&[("Books", true)]));
        shell.change_dir("books").unwrap();
        assert_eq!(shell.pwd(), "/Tablet/Books");
    }

    #[test]
    fn cd_to_unknown_storage_fails() {
        let mut shell = shell_with(&["Internal", "SDCard"], vec![]);
        let err = shell.change_dir("Nope").unwrap_err();
        assert!(err.to_string().contains("no such storage"), "{err}");
    }

    #[test]
    fn absolute_cd_resets_to_root() {
        let mut shell = shell_with(&["Tablet"], listing(&[("Books", true)]));
        shell.change_dir("Books").unwrap();
        shell.change_dir("/").unwrap();
        assert_eq!(shell.pwd(), "/Tablet"); // single storage: root is inside it
    }

    #[test]
    fn save_restore_roundtrips_location() {
        let mut shell = shell_with(&["Tablet"], listing(&[("Books", true)]));
        let saved = shell.save();
        shell.change_dir("Books").unwrap();
        assert_eq!(shell.pwd(), "/Tablet/Books");
        shell.restore(saved);
        assert_eq!(shell.pwd(), "/Tablet");
    }

    #[test]
    fn completion_candidates_reflect_directory_contents() {
        let mut shell = shell_with(
            &["Tablet"],
            listing(&[("Books", true), ("readme.txt", false)]),
        );
        let candidates = shell.completion_candidates().unwrap();
        assert_eq!(
            candidates,
            vec![
                ("Books".to_string(), true),
                ("readme.txt".to_string(), false)
            ]
        );
    }

    #[test]
    fn storage_names_fall_back_to_description() {
        // Empty volume label: description is used instead.
        let mut w = Writer::new();
        w.u16(3);
        w.u16(2);
        w.u16(0);
        w.u64(1000);
        w.u64(500);
        w.u32(0);
        w.string("Internal shared storage").unwrap();
        w.string("").unwrap();
        let mut ids = Writer::new();
        ids.u32(1);
        ids.u32(0x10001);
        let script = vec![
            ok(),
            data(op::GET_STORAGE_IDS, &ids.into_inner()),
            ok(),
            data(op::GET_STORAGE_INFO, &w.into_inner()),
            ok(),
        ];
        let session = Session::open(MockPipe {
            reads: RefCell::new(script.into()),
        })
        .unwrap();
        let shell = Shell::new(session).unwrap();
        assert_eq!(shell.pwd(), "/Internal shared storage");
    }
}
