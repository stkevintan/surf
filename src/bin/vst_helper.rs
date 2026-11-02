//! Out-of-process VST3 host.
//!
//! Started by `surf.exe` as `surf-vst-host.exe`.  The plugin, its GUI and its
//! native threads live only in this process; if Kontakt aborts, the UI process
//! survives.  Audio/MIDI travel through shared memory; control/state/render
//! travel over stdin/stdout JSON lines.

use std::io::{BufRead, BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use surf::audio::Instrument;
use surf::audio::ipc::{MidiKind, SharedMap};
use surf::audio::render::{RenderOptions, render_with_instrument};
use surf::audio::vst::{VstPlugin, render_vst_to_wav};
use surf::audio::vst2::{Vst2EditorWindow, Vst2Plugin};
use surf::audio::vst_client::{HelperEvent, HelperRequest, RenderNote};
use surf::song::{Timeline, TimedNote};
use vst3_host::window::PluginWindow;
use windows::core::HSTRING;
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows::Win32::System::LibraryLoader::SetDllDirectoryW;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

/// Initialise COM on the thread that will own the plugin and its editor.
///
/// Plugin GUIs are Win32/OLE citizens: Kontakt's browser, its file dialogs and NI's NTK
/// widgets all call into COM, and several of them query WMI (which needs an initialised
/// apartment — the `Win32_ComputerSystemProduct` lookup failures in the helper log are this).
/// A host that never calls `CoInitializeEx` leaves that behaviour undefined.
///
/// Single-threaded apartment is what a UI-owning thread needs (and what every Windows host
/// does); `COINIT_DISABLE_OLE1DDE` just skips legacy OLE1 support we have no use for.
/// `RPC_E_CHANGED_MODE` means something already initialised this thread as MTA — worth a
/// warning, but not fatal, so we carry on rather than refusing to load.
fn init_com() -> bool {
    // SAFETY: called once, on the main thread, before any plugin or COM object exists.
    let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    if hr.is_ok() {
        eprintln!("helper: COM initialised (STA)");
        true
    } else {
        eprintln!("helper: CoInitializeEx failed: {hr:?} (continuing without COM init)");
        false
    }
}

enum Backend {
    Vst3(VstPlugin),
    Vst2(Vst2Plugin),
}

/// A panic in the helper can be the last thing the process ever writes: the
/// host reads stderr asynchronously and may miss the tail when the process
/// dies. Mirror panics into `%TEMP%\surf-vst-host-panic.log` so a 0/101 exit
/// is diagnosable after the fact.
fn install_panic_log() {
    let fallback = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let text = format!("surf-vst-host panic: {info}");
        eprintln!("{text}");
        let path = surf::paths::data_dir().join("surf-vst-host-panic.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(file, "{text}");
        }
        fallback(info);
    }));
}

#[allow(dead_code)]
enum EditorWindow {
    Vst3(PluginWindow),
    Vst2(Vst2EditorWindow),
}

/// Balances [`init_com`] on drop.
///
/// Declared before the plugin in `main` so Rust's reverse drop order tears COM down *after*
/// the plugin released its COM objects — uninitialising the apartment while a plugin still
/// holds interfaces is its own crash.
struct ComGuard(bool);

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: pairs with the successful CoInitializeEx on this same thread.
            unsafe { CoUninitialize() };
        }
    }
}

impl Backend {
    fn name(&self) -> &str {
        match self {
            Self::Vst3(v) => &v.name,
            Self::Vst2(v) => &v.name,
        }
    }

    fn instrument(&self) -> Box<dyn Instrument> {
        match self {
            Self::Vst3(v) => Box::new(v.instrument()),
            Self::Vst2(v) => Box::new(v.instrument()),
        }
    }

    fn open_editor(&self) -> Result<EditorWindow> {
        match self {
            Self::Vst3(v) => Ok(EditorWindow::Vst3(v.open_editor()?)),
            Self::Vst2(v) => Ok(EditorWindow::Vst2(v.open_editor()?)),
        }
    }

    fn state(&self) -> Result<Vec<u8>> {
        match self {
            Self::Vst3(v) => v.state(),
            Self::Vst2(v) => v.state(),
        }
    }

    fn load_state(&self, bytes: &[u8]) -> Result<()> {
        match self {
            Self::Vst3(v) => v.load_state(bytes),
            Self::Vst2(v) => v.load_state(bytes),
        }
    }
}

struct Args {
    shm: String,
    plugin: PathBuf,
    sample_rate: u32,
    state: Option<PathBuf>,
}

fn main() -> Result<()> {
    install_panic_log();
    eprintln!("helper: starting");
    // Must happen before the plugin is loaded, and must outlive it (see ComGuard).
    let _com = ComGuard(init_com());
    let args = parse_args()?;
    eprintln!("helper: opening shared memory {}", args.shm);
    let map = Arc::new(SharedMap::open(&args.shm)?);
    let state = args
        .state
        .as_ref()
        .and_then(|path| std::fs::read(path).ok());

    // The portable Kontakt wrapper (`KtpVST3`) loads `ktp871.dll` from the
    // parent of its VST3 folder via a bare LoadLibrary("ktp871.dll").  A normal
    // host's DLL search path does not include that folder, so the wrapper
    // crashes with an access violation.  Run the helper from the plugin's
    // parent directory, matching what the standalone launcher does.
    if let Some(search_dir) = plugin_search_dir(&args.plugin) {
        if let Err(err) = std::env::set_current_dir(search_dir) {
                eprintln!(
                    "helper: could not chdir to {}: {err}",
                    search_dir.display()
                );
            } else {
                eprintln!("helper: cwd set to {}", search_dir.display());
                let wide = HSTRING::from(search_dir.to_string_lossy().as_ref());
                if let Err(err) = unsafe { SetDllDirectoryW(&wide) } {
                    eprintln!("helper: SetDllDirectoryW failed: {err}");
                } else {
                    eprintln!("helper: DLL directory set to {}", search_dir.display());
                }
            }
    }

    eprintln!("helper: loading plugin {}", args.plugin.display());
    let is_vst2 = args
        .plugin
        .extension()
        .and_then(|s| s.to_str())
        .is_some_and(|s| s.eq_ignore_ascii_case("dll"));
    let backend = if is_vst2 {
        Backend::Vst2(
            Vst2Plugin::load_with_state(&args.plugin, args.sample_rate, state.as_deref())
                .context("helper failed to load VST2 plugin")?,
        )
    } else {
        Backend::Vst3(
            VstPlugin::load_with_state(&args.plugin, args.sample_rate, state.as_deref())
                .context("helper failed to load VST3 plugin")?,
        )
    };
    eprintln!("helper: plugin loaded: {}", backend.name());
    let name = backend.name().to_string();
    map.set_helper_alive(true);

    let stop = Arc::new(AtomicBool::new(false));
    let instrument = backend.instrument();
    let audio_map = map.clone();
    let audio_stop = stop.clone();
    let sample_rate = args.sample_rate;
    let audio_thread = std::thread::Builder::new()
        .name("surf-vst-audio".into())
        .spawn(move || audio_loop(instrument, audio_map, audio_stop))
        .context("could not spawn helper audio thread")?;

    let mut out = BufWriter::new(std::io::stdout());
    send_event(
        &mut out,
        &HelperEvent::Ready {
            name,
            sample_rate,
        },
    )?;

    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("surf-vst-control".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else {
                    break;
                };
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<HelperRequest>(line) {
                    Ok(req) => {
                        if tx.send(req).is_err() {
                            break;
                        }
                    }
                    Err(err) => eprintln!("helper: bad request: {err}"),
                }
            }
            // Parent exited / closed the pipe.
            let _ = tx.send(HelperRequest::Shutdown);
        })
        .context("could not spawn helper control thread")?;

    let mut editor: Option<EditorWindow> = None;
    let mut running = true;
    while running {
        pump_messages();
        if let Some(EditorWindow::Vst2(window)) = &mut editor {
            window.idle();
        }
        while let Ok(request) = rx.try_recv() {
            match request {
                HelperRequest::OpenEditor => match backend.open_editor() {
                    Ok(window) => {
                        editor = Some(window);
                        send_event(&mut out, &HelperEvent::EditorOpened)?;
                    }
                    Err(err) => send_error(&mut out, format!("{err:#}"))?,
                },
                HelperRequest::CloseEditor => {
                    editor = None;
                    send_event(&mut out, &HelperEvent::EditorOpened)?;
                }
                HelperRequest::SaveState { path } => match backend.state() {
                    Ok(bytes) => match std::fs::write(&path, &bytes) {
                        Ok(()) => send_event(
                            &mut out,
                            &HelperEvent::StateSaved {
                                bytes: bytes.len(),
                                path,
                            },
                        )?,
                        Err(err) => send_error(&mut out, format!("{err}"))?,
                    },
                    Err(err) => send_error(&mut out, format!("{err:#}"))?,
                },
                HelperRequest::LoadState { path } => match std::fs::read(&path) {
                    Ok(bytes) => match backend.load_state(&bytes) {
                        Ok(()) => send_event(&mut out, &HelperEvent::StateLoaded)?,
                        Err(err) => send_error(&mut out, format!("{err:#}"))?,
                    },
                    Err(err) => send_error(&mut out, format!("{err}"))?,
                },
                HelperRequest::Render {
                    output,
                    sample_rate,
                    notes,
                    duration,
                    tail_secs,
                } => {
                    let state = backend.state().ok();
                    let result = match &backend {
                        Backend::Vst3(_) => render_to_wav(
                            &args.plugin,
                            state.as_deref(),
                            &notes,
                            duration,
                            &output,
                            sample_rate,
                            tail_secs,
                        ),
                        Backend::Vst2(_) => render_vst2_to_wav(
                            &args.plugin,
                            state.as_deref(),
                            &notes,
                            duration,
                            &output,
                            sample_rate,
                            tail_secs,
                        ),
                    };
                    match result {
                        Ok((path, duration, peak)) => send_event(
                            &mut out,
                            &HelperEvent::RenderDone {
                                path,
                                duration,
                                peak,
                            },
                        )?,
                        Err(err) => send_error(&mut out, format!("{err:#}"))?,
                    }
                }
                HelperRequest::Ping => send_event(&mut out, &HelperEvent::Pong)?,
                HelperRequest::Shutdown => running = false,
            }
        }
        if !map.main_alive() {
            running = false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    stop.store(true, Ordering::Release);
    let _ = audio_thread.join();
    drop(editor);
    map.set_helper_alive(false);
    Ok(())
}

fn plugin_search_dir(plugin: &std::path::Path) -> Option<&std::path::Path> {
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

fn parse_args() -> Result<Args> {
    let mut shm = None;
    let mut plugin = None;
    let mut sample_rate = 48_000;
    let mut state = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--shm" => shm = args.next(),
            "--plugin" => plugin = args.next().map(PathBuf::from),
            "--sample-rate" => {
                sample_rate = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(48_000);
            }
            "--state" => state = args.next().map(PathBuf::from),
            other => eprintln!("helper: ignoring argument {other}"),
        }
    }
    Ok(Args {
        shm: shm.context("missing --shm")?,
        plugin: plugin.context("missing --plugin")?,
        sample_rate,
        state,
    })
}

fn send_event(out: &mut BufWriter<std::io::Stdout>, event: &HelperEvent) -> Result<()> {
    serde_json::to_writer(&mut *out, event)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn send_error(out: &mut BufWriter<std::io::Stdout>, message: String) -> Result<()> {
    eprintln!("helper error: {message}");
    send_event(out, &HelperEvent::Error { message })
}

fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Register this thread with MMCSS so Windows schedules it as audio work.
///
/// Returns the task handle, which must stay alive for the registration to hold.
///
/// Without this the render thread is an ordinary thread: the scheduler can park it
/// for several milliseconds, which forces a large queue cushion just to survive a
/// missed wakeup, and that cushion *is* the latency the player feels. "Pro Audio" is
/// the same MMCSS task every DAW and WASAPI's own engine thread uses.
#[cfg(windows)]
fn join_pro_audio_mmcss() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW;
    use windows::core::w;

    let mut task_index = 0u32;
    // SAFETY: called once from the thread being registered; `task_index` is a live out-param.
    match unsafe { AvSetMmThreadCharacteristicsW(w!("Pro Audio"), &mut task_index) } {
        Ok(handle) if !handle.is_invalid() => {
            eprintln!("helper: audio thread joined MMCSS 'Pro Audio'");
            Some(handle)
        }
        Ok(_) => None,
        Err(err) => {
            // Not fatal: the cushion still covers scheduling jitter, just less tightly.
            eprintln!("helper: MMCSS registration failed ({err}); using default scheduling");
            None
        }
    }
}

/// Render audio into the shared ring, keeping it topped up to a fixed cushion.
///
/// The consumer's drain rate sets the pace, so this needs no clock of its own.
fn audio_loop(mut instrument: Box<dyn Instrument>, map: Arc<SharedMap>, stop: Arc<AtomicBool>) {
    // Ask for audio-class scheduling before sizing the cushion: the whole point is
    // that a reliably-woken thread needs far less queued audio to stay glitch-free.
    #[cfg(windows)]
    let _mmcss = join_pro_audio_mmcss();

    // Make `sleep(1ms)` mean roughly 1 ms. The default timer granularity is ~15.6 ms,
    // which would let the ring run dry no matter how the loop is written. Paired with
    // `timeEndPeriod` when the loop exits.
    #[cfg(windows)]
    let timer_period = {
        use windows::Win32::Media::timeBeginPeriod;
        // SAFETY: plain system call; balanced by timeEndPeriod below.
        let ok = unsafe { timeBeginPeriod(1) } == 0;
        if !ok {
            eprintln!("helper: timeBeginPeriod(1) failed; sleeps may be coarse");
        }
        ok
    };

    // How much audio to keep queued ahead of the consumer.
    //
    // This is the latency the player feels, so it should be as small as the
    // scheduler allows. The consumer takes one device buffer per callback (480
    // frames / 10 ms here); the cushion has to cover one callback plus whatever
    // jitter remains, or the ring runs dry mid-buffer and the silence is heard as
    // crackle.
    //
    // 512 frames = 10.7 ms: one 480-frame device buffer plus a small margin. Measured
    // glitch-free here (0 underruns over 800 buffers) thanks to the MMCSS registration
    // and 1 ms timer above; 1024 was conservative and cost ~10 ms for nothing.
    // Going below one device buffer cannot work: the consumer takes 480 frames at once.
    // `SURF_AUDIO_CUSHION` overrides it in frames if another machine needs more slack.
    let target_queue: u64 = std::env::var("SURF_AUDIO_CUSHION")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| (128..=16_384).contains(v))
        .unwrap_or(512);
    eprintln!(
        "helper: audio cushion {target_queue} frames ({:.1} ms @ 48k)",
        target_queue as f64 * 1000.0 / 48_000.0
    );

    let mut buffer = [0.0f32; 128];
    while !stop.load(Ordering::Acquire) {
        while let Some(msg) = map.pop_midi() {
            match msg.kind {
                k if k == MidiKind::NoteOn as u32 => {
                    instrument.note_on(msg.a as u8, msg.b as u8)
                }
                k if k == MidiKind::NoteOff as u32 => instrument.note_off(msg.a as u8),
                k if k == MidiKind::Sustain as u32 => instrument.set_sustain(msg.a != 0),
                k if k == MidiKind::AllNotesOff as u32 => instrument.all_notes_off(),
                _ => {}
            }
        }
        // Top the queue back up to the cushion.
        //
        // Deliberately *not* derived from elapsed time: the loop renders whole
        // 64-frame blocks, so any time-derived target leaves a sub-block remainder
        // that gets rounded away every pass. That rounding loss accumulates into a
        // helper which renders slower than real time, starving the consumer. Asking
        // "how far below the cushion is the queue?" is self-correcting — the
        // consumer's own drain rate sets the pace, and the queue depth is the only
        // thing that needs to stay bounded.
        let queued = map.audio_backlog();
        let target = target_queue.saturating_sub(queued);

        let mut rendered = 0u64;
        while rendered + 64 <= target {
            instrument.process(&mut buffer, 64);
            if !map.push_audio(&buffer) {
                // Consumer stalled (long GUI operation): drop the block rather
                // than blocking the audio thread.
                break;
            }
            rendered += 64;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    instrument.all_notes_off();

    #[cfg(windows)]
    if timer_period {
        use windows::Win32::Media::timeEndPeriod;
        // SAFETY: balances the successful timeBeginPeriod(1) above.
        unsafe {
            timeEndPeriod(1);
        }
    }
}

fn timeline_from_notes(notes: &[RenderNote], duration: f64) -> Timeline {
    Timeline {
        notes: notes
            .iter()
            .map(|n| TimedNote {
                key: n.key,
                velocity: n.velocity,
                channel: n.channel,
                start: n.start,
                end: n.end,
            })
            .collect(),
        duration,
    }
}

fn render_to_wav(
    plugin: &std::path::Path,
    state: Option<&[u8]>,
    notes: &[RenderNote],
    duration: f64,
    output: &str,
    sample_rate: u32,
    tail_secs: f64,
) -> Result<(String, f64, f32)> {
    let timeline = timeline_from_notes(notes, duration);
    let stats = render_vst_to_wav(
        plugin,
        state,
        &timeline,
        output,
        sample_rate,
        &RenderOptions {
            sample_rate,
            tail_secs,
            gain: 0.9,
        },
    )?;
    Ok((stats.path, stats.duration, stats.peak))
}

fn render_vst2_to_wav(
    plugin: &std::path::Path,
    state: Option<&[u8]>,
    notes: &[RenderNote],
    duration: f64,
    output: &str,
    sample_rate: u32,
    tail_secs: f64,
) -> Result<(String, f64, f32)> {
    let timeline = timeline_from_notes(notes, duration);
    let vst = Vst2Plugin::load_with_state(plugin, sample_rate, state)?;
    let mut instrument = vst.instrument();
    let stats = render_with_instrument(
        &timeline,
        &mut instrument,
        output,
        &RenderOptions {
            sample_rate,
            tail_secs,
            gain: 0.9,
        },
    )?;
    Ok((stats.path, stats.duration, stats.peak))
}

// Keep the receiver type explicit for readability of the control loop.
#[allow(dead_code)]
fn _assert_receiver(_: Receiver<HelperRequest>) {}
