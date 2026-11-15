//! Client side of the out-of-process VST3 host.
//!
//! Control/state/render go over line-delimited JSON on the helper's
//! stdin/stdout; real-time audio and MIDI use the shared-memory ring in
//! [`crate::shm`].

use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

pub use crate::protocol::{HelperEvent, HelperRequest, RenderNote};
use crate::remote::RemoteInstrument;
use crate::shm::SharedMap;

pub struct VstClient {
    pub path: PathBuf,
    pub name: String,
    map: Arc<SharedMap>,
    child: Child,
    stdin: Mutex<BufWriter<ChildStdin>>,
    events: Receiver<HelperEvent>,
    /// This helper's private `%TEMP%`, removed once the helper has exited.
    temp_dir: Option<PathBuf>,
}

impl VstClient {
    pub fn load(
        plugin: impl AsRef<Path>,
        sample_rate: u32,
        state_path: Option<&Path>,
    ) -> Result<Self> {
        let plugin =
            std::path::absolute(plugin.as_ref()).unwrap_or_else(|_| plugin.as_ref().to_path_buf());
        let helper = helper_path()?;
        let shm_name = crate::shm::make_name();
        let map = Arc::new(SharedMap::create(&shm_name)?);
        map.set_sample_rate(sample_rate);

        let mut command = Command::new(&helper);
        command
            .arg("--shm")
            .arg(&shm_name)
            .arg("--plugin")
            .arg(&plugin)
            .arg("--sample-rate")
            .arg(sample_rate.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        // The helper's stderr goes to a file, not a pipe: Kontakt 8.14 aborts
        // (`0x40000015`) during load when its log output is a pipe. The file
        // also survives the helper, so a crash is diagnosable afterwards.
        // `SURF_HELPER_LOG` only overrides the location.
        let log_path = helper_log_path();
        // Where this run's output starts, so a failure can replay just its own lines to
        // stderr instead of the whole accumulated log (see `log_since`).
        let log_offset = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
        let log_ok = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map(|mut file| {
                use std::io::Write as _;
                let _ = writeln!(file, "\n--- helper for {} ---", plugin.display());
                file
            });
        match log_ok {
            Ok(file) => {
                command.stderr(Stdio::from(file));
            }
            Err(_) => {
                command.stderr(Stdio::piped());
            }
        }
        if let Some(state) = state_path {
            command.arg("--state").arg(state);
        }
        // Give the helper its own empty %TEMP%.
        //
        // Kontakt statically links libzmq 4.3.5, whose `create_ipc_wildcard_address`
        // does `_wtmpnam_s` + `_wmkdir` and *ignores the result*.  When that mkdir
        // fails, `signaler_t` leaves its `_r`/`_w` descriptors uninitialised, and the
        // garbage value reaches `epoll_ctl(EPOLL_CTL_ADD)`, which fails EBADF and
        // trips `errno_assert` at epoll.cpp:73 -> abort (`0x40000015`).  Upstream
        // fixed the unchecked call in Aug 2024, after 4.3.5 shipped, so the only
        // lever a host has is making sure that mkdir succeeds.
        //
        // In this user's real `%TEMP%` (900+ entries) `_wmkdir` returns
        // ACCESS_DENIED for every name `_wtmpnam_s` produces; in a fresh directory
        // it succeeds and Kontakt loads.  Note `tmpnam_s` caches the temp path on
        // first use, so this has to be set before the process starts — it cannot be
        // fixed from inside the helper.
        let temp_dir = helper_temp_dir();
        if let Some(tmp) = &temp_dir {
            command.env("TMP", tmp).env("TEMP", tmp);
        }
        // Start the helper in the plugin's parent directory (for Kontakt
        // Portable this is `...\Kontakt 8\x64`, next to ktp871.dll).  Doing
        // this at process creation is more reliable than only chdir-ing later.
        if let Some(search_dir) = plugin_search_dir(&plugin) {
            command.current_dir(search_dir);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("could not start VST helper {}", helper.display()))?;

        let stdin = child
            .stdin
            .take()
            .context("helper stdin was not captured")?;
        let stdout = child
            .stdout
            .take()
            .context("helper stdout was not captured")?;
        if let Some(stderr) = child.stderr.take() {
            std::thread::Builder::new()
                .name("surf-vst-helper-stderr".into())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        eprintln!("vst-helper: {line}");
                    }
                })
                .ok();
        }
        let events = spawn_reader(stdout);

        let mut client = Self {
            path: plugin,
            name: "VST helper".to_string(),
            map,
            child,
            stdin: Mutex::new(BufWriter::new(stdin)),
            events,
            temp_dir,
        };

        // Kontakt 8.14 takes ~80s to instantiate on a cold cache on this machine
        // (it scans libraries and talks to NTKDaemon before the factory returns),
        // so 30s was timing out on a load that would have succeeded. Allow a wide
        // margin: a plugin that is merely slow should load, not be killed.
        match client.wait_event(Duration::from_secs(240), |ev| {
            matches!(ev, HelperEvent::Ready { .. })
        }) {
            Ok(HelperEvent::Ready { name, .. }) => {
                client.name = name;
                Ok(client)
            }
            Ok(_) => anyhow::bail!("unexpected helper handshake"),
            Err(err) => {
                // `Exited` can arrive before the child is waitable; poll briefly
                // so the UI gets the actual exit code instead of a generic
                // "process exited".
                let deadline = Instant::now() + Duration::from_millis(500);
                let mut status = client.child.try_wait().ok().flatten();
                while status.is_none() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(10));
                    status = client.child.try_wait().ok().flatten();
                }
                let _ = client.child.kill();
                let _ = client.child.wait();
                // The helper's stderr went to a file (it must not be a pipe — see above), so
                // nothing reached the terminal. Replay this run's portion now: an exit code
                // alone rarely says why, and the surrounding lines usually do.
                let code = status.and_then(|s| s.code());
                eprintln!("---- VST helper did not come up ----");
                eprintln!("plugin: {}", client.path.display());
                eprintln!("error : {err:#}");
                if let Some(code) = code {
                    eprintln!("exit  : {code} ({code:#x})");
                }
                let transcript = log_since(&log_path, log_offset);
                if transcript.trim().is_empty() {
                    eprintln!("helper wrote nothing to {}", log_path.display());
                } else {
                    eprintln!("helper output:");
                    for line in transcript.lines() {
                        eprintln!("  {line}");
                    }
                }
                eprintln!("------------------------------------");
                match code {
                    Some(code) => Err(anyhow::anyhow!("{err:#} (helper exit code {code})")),
                    None => Err(err),
                }
            }
        }
    }

    pub fn instrument(&self) -> RemoteInstrument {
        RemoteInstrument::new(self.map.clone(), self.name.clone())
    }

    /// The shared mapping, for diagnostics (see `examples/ring_backlog.rs`).
    pub fn shared_map(&self) -> Arc<SharedMap> {
        self.map.clone()
    }

    pub fn open_editor(&mut self) -> Result<()> {
        self.request(&HelperRequest::OpenEditor)?;
        self.wait_event(Duration::from_secs(15), |ev| {
            matches!(ev, HelperEvent::EditorOpened)
        })?;
        Ok(())
    }

    pub fn close_editor(&mut self) -> Result<()> {
        self.request(&HelperRequest::CloseEditor)?;
        self.wait_event(Duration::from_secs(15), |ev| {
            matches!(ev, HelperEvent::EditorClosed)
        })?;
        Ok(())
    }

    pub fn save_state(&mut self, path: &Path) -> Result<usize> {
        self.request(&HelperRequest::SaveState {
            path: path.display().to_string(),
        })?;
        match self.wait_event(Duration::from_secs(120), |ev| {
            matches!(ev, HelperEvent::StateSaved { .. })
        })? {
            HelperEvent::StateSaved { bytes, .. } => Ok(bytes),
            _ => anyhow::bail!("unexpected save-state response"),
        }
    }

    pub fn load_state(&mut self, path: &Path) -> Result<()> {
        self.request(&HelperRequest::LoadState {
            path: path.display().to_string(),
        })?;
        self.wait_event(Duration::from_secs(120), |ev| {
            matches!(ev, HelperEvent::StateLoaded)
        })?;
        Ok(())
    }

    pub fn render(
        &mut self,
        output: &Path,
        notes: Vec<RenderNote>,
        duration: f64,
        sample_rate: u32,
        tail_secs: f64,
        controls: Vec<(u8, u8, u8)>,
    ) -> Result<(String, f64, f32)> {
        let output = std::path::absolute(output).unwrap_or_else(|_| output.to_path_buf());
        self.request(&HelperRequest::Render {
            output: output.display().to_string(),
            sample_rate,
            notes,
            duration,
            tail_secs,
            controls,
        })?;
        match self.wait_event(Duration::from_secs(600), |ev| {
            matches!(ev, HelperEvent::RenderDone { .. })
        })? {
            HelperEvent::RenderDone {
                path,
                duration,
                peak,
            } => Ok((path, duration, peak)),
            _ => anyhow::bail!("unexpected render response"),
        }
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn request(&mut self, request: &HelperRequest) -> Result<()> {
        let mut line = serde_json::to_vec(request)?;
        line.push(b'\n');
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| anyhow::anyhow!("helper stdin poisoned"))?;
        stdin.write_all(&line)?;
        stdin.flush()?;
        Ok(())
    }

    fn wait_event<F>(&mut self, timeout: Duration, wanted: F) -> Result<HelperEvent>
    where
        F: Fn(&HelperEvent) -> bool,
    {
        let started = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                anyhow::bail!("VST helper did not respond in time");
            }
            match self.events.recv_timeout(remaining) {
                Ok(HelperEvent::Error { message }) => anyhow::bail!("VST helper: {message}"),
                Ok(HelperEvent::Exited) => {
                    let status = self
                        .child
                        .try_wait()
                        .ok()
                        .flatten()
                        .map(|s| format!(" (exit {})", s.code().unwrap_or(-1)))
                        .unwrap_or_default();
                    anyhow::bail!("VST helper process exited{status}")
                }
                Ok(event) => {
                    if wanted(&event) {
                        return Ok(event);
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    anyhow::bail!("VST helper did not respond in time")
                }
                Err(RecvTimeoutError::Disconnected) => {
                    anyhow::bail!("VST helper channel closed")
                }
            }
        }
    }
}

impl Drop for VstClient {
    fn drop(&mut self) {
        let _ = self.request(&HelperRequest::Shutdown);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut exited = false;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if !exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.map.set_main_alive(false);
        }
        if let Some(dir) = &self.temp_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

fn plugin_search_dir(plugin: &Path) -> Option<&Path> {
    // Escape hatch for A/B testing a plugin that misbehaves when the helper
    // runs from a different working directory.
    if std::env::var_os("SURF_NO_CHDIR").is_some() {
        return None;
    }
    let parent = plugin.parent()?;
    let name = parent.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if name.eq_ignore_ascii_case("VST") || name.eq_ignore_ascii_case("VST3") {
        parent.parent().or(Some(parent))
    } else {
        Some(parent)
    }
}

/// Read the helper log from `offset` to the end — i.e. only the current run's output.
///
/// The log is append-only and shared by every run, so the byte offset captured before spawn
/// is what separates "this failure" from the whole history. Decoded lossily on purpose: the
/// plugin writes in the system codepage, and a readable approximation beats failing here.
/// (`app.rs` does a proper GBK decode for what it shows in the UI.)
fn log_since(path: &Path, offset: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A private, empty `%TEMP%` for one helper process.
///
/// See the call site for why Kontakt needs this. libzmq's `_wtmpnam_s` names are short
/// and derived from process state, so leftovers from an earlier run are what turn into
/// a collision later. Each helper therefore gets a fresh directory of its own under
/// `helper-tmp`, named `<ui pid>-<n>`. Several helpers run at once (one per plugin), so
/// a spawn must not touch the directories of helpers that are still running; it only
/// clears what earlier Surf runs left behind. Returns `None` if the directory cannot
/// be prepared, in which case the helper simply inherits the ambient `%TEMP%`.
fn helper_temp_dir() -> Option<PathBuf> {
    if std::env::var_os("SURF_NO_PRIVATE_TEMP").is_some() {
        return None;
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    new_helper_temp_dir(
        &surf_platform::data_dir().join("helper-tmp"),
        std::process::id(),
        n,
    )
}

fn new_helper_temp_dir(root: &Path, pid: u32, n: u64) -> Option<PathBuf> {
    std::fs::create_dir_all(root).ok()?;
    let own = format!("{pid}-");
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with(&own) {
                continue; // a helper of this run, possibly still running
            }
            // Best effort: a file we cannot delete is not worth failing the spawn over.
            let path = entry.path();
            let _ = if path.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
        }
    }
    let dir = root.join(format!("{pid}-{n}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).ok()?;
    Some(dir)
}

/// Where the helper writes its stderr log (see [`VstClient::load`]).
///
/// Defaults to `surf-vst-host.log` in the writable data directory (see
/// [`crate::paths::data_dir`]); `SURF_HELPER_LOG` overrides it.
pub fn helper_log_path() -> PathBuf {
    std::env::var_os("SURF_HELPER_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| surf_platform::data_dir().join("surf-vst-host.log"))
}

fn helper_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("could not locate current exe")?;
    let dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current exe has no directory"))?;
    let name = if cfg!(windows) {
        "surf-vst-host.exe"
    } else {
        "surf-vst-host"
    };
    // Normal app: target/debug/surf-vst-host.exe. Examples live one directory
    // deeper (target/debug/examples), so also check the parent.
    let mut candidates = vec![dir.join(name)];
    if let Some(parent) = dir.parent() {
        candidates.push(parent.join(name));
    }
    for helper in candidates {
        if helper.exists() {
            return Ok(helper);
        }
    }
    anyhow::bail!(
        "VST helper not found next to {} (build it with `cargo build`)",
        exe.display()
    )
}

fn spawn_reader(stdout: ChildStdout) -> Receiver<HelperEvent> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("surf-vst-helper-events".into())
        .spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let Ok(line) = line else {
                    break;
                };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<HelperEvent>(line) {
                    Ok(event) => {
                        let _ = tx.send(event);
                    }
                    Err(_) => {
                        // Never let plugin chatter poison the protocol.
                        eprintln!("vst-helper: {line}");
                    }
                }
            }
            let _ = tx.send(HelperEvent::Exited);
        })
        .expect("failed to spawn helper reader thread");
    rx
}

#[cfg(test)]
mod tests {
    use super::new_helper_temp_dir;

    /// Starting a second helper must leave the first one's `%TEMP%` alone, and only
    /// clear what earlier runs of Surf left behind.
    #[test]
    fn each_helper_gets_its_own_temp_dir() {
        let root = std::env::temp_dir().join(format!("surf-helper-tmp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("1-0")).unwrap();
        std::fs::write(root.join("1-0").join("stale"), b"x").unwrap();
        std::fs::write(root.join("loose-file-from-old-builds"), b"x").unwrap();

        let first = new_helper_temp_dir(&root, 42, 0).unwrap();
        std::fs::write(first.join("zmq-ipc"), b"in use").unwrap();
        let second = new_helper_temp_dir(&root, 42, 1).unwrap();

        assert_ne!(first, second);
        assert!(first.join("zmq-ipc").exists(), "a running helper's files survive");
        assert!(second.read_dir().unwrap().next().is_none(), "a new helper starts empty");
        assert!(!root.join("1-0").exists(), "an earlier run's directory is cleared");
        assert!(!root.join("loose-file-from-old-builds").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
