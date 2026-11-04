//! Out-of-process plugin host.
//!
//! Started by the Surf app as `surf-vst-host`, one process per plugin.  The plugin, its GUI and its
//! native threads live only in this process; if Kontakt aborts, the UI process
//! survives.  Audio/MIDI travel through shared memory; control/state/render
//! travel over stdin/stdout JSON lines.

mod protocol_out;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use surf_audio::Instrument;
use surf_audio::render::{RenderOptions, render_with_instrument};
use surf_core::song::{TimedNote, Timeline};
use surf_plugin::backends::vst3::{VstPlugin, render_vst_to_wav};
use surf_plugin::backends::vst2::{Vst2EditorWindow, Vst2Plugin};
use surf_plugin::format::{self, Format};
use surf_plugin::protocol::{HelperEvent, HelperRequest, RenderNote};
use surf_plugin::shm::{MidiKind, SharedMap};
use vst3_host::window::PluginWindow;
#[cfg(windows)]
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
#[cfg(windows)]
use windows::Win32::System::LibraryLoader::SetDllDirectoryW;
#[cfg(windows)]
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};
#[cfg(windows)]
use windows::core::HSTRING;

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
#[cfg(windows)]
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

#[cfg(not(windows))]
fn init_com() -> bool {
    false
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
        let path = surf_platform::data_dir().join("surf-vst-host-panic.log");
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
// The flag is only read on Windows, where COM exists.
struct ComGuard(#[cfg_attr(not(windows), allow(dead_code))] bool);

impl Drop for ComGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
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
    #[cfg(target_os = "macos")]
    appkit::init();
    // Before any plugin is loaded: plugins print to stdout, which must not
    // mix with protocol messages (see protocol_out).
    let mut out = protocol_out::take();
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
                #[cfg(windows)]
                {
                    let wide = HSTRING::from(search_dir.to_string_lossy().as_ref());
                    if let Err(err) = unsafe { SetDllDirectoryW(&wide) } {
                        eprintln!("helper: SetDllDirectoryW failed: {err}");
                    } else {
                        eprintln!("helper: DLL directory set to {}", search_dir.display());
                    }
                }
            }
    }

    eprintln!("helper: loading plugin {}", args.plugin.display());
    let backend = match format::detect(&args.plugin) {
        Some(Format::Vst2) => Backend::Vst2(
            Vst2Plugin::load_with_state(&args.plugin, args.sample_rate, state.as_deref())
                .context("helper failed to load VST2 plugin")?,
        ),
        // VST3 is the default: a `.vst3` path, or anything vst3-host can open.
        _ => Backend::Vst3(
            VstPlugin::load_with_state(&args.plugin, args.sample_rate, state.as_deref())
                .context("helper failed to load VST3 plugin")?,
        ),
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
    // Debug hook for the `editor_reopen` probe: simulate the user closing the
    // editor with its title-bar button this many milliseconds after it opens.
    #[cfg(target_os = "macos")]
    let test_close_after = std::env::var("SURF_TEST_CLOSE_EDITOR_AFTER_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis);
    #[cfg(target_os = "macos")]
    let mut editor_opened_at: Option<std::time::Instant> = None;
    let mut running = true;
    while running {
        pump_messages();
        #[cfg(target_os = "macos")]
        if let (Some(after), Some(at)) = (test_close_after, editor_opened_at)
            && at.elapsed() >= after
        {
            editor_opened_at = None;
            appkit::perform_close_all();
        }
        match &mut editor {
            Some(EditorWindow::Vst2(window)) => window.idle(),
            Some(EditorWindow::Vst3(window)) => {
                // The title-bar close button hides the window but tells neither the
                // plugin nor us. Drop it so the plugin's view is detached; otherwise
                // the next OpenEditor fails with "Editor already open".
                if window.closed_by_user() {
                    editor = None;
                    eprintln!("helper: editor closed by the user");
                    send_event(&mut out, &HelperEvent::EditorClosed)?;
                } else if let Err(err) = window.service_platform_events() {
                    // Applies plugin-initiated resizes (Kontakt sizes itself after attach).
                    eprintln!("helper: editor event servicing failed: {err}");
                }
            }
            None => {}
        }
        while let Ok(request) = rx.try_recv() {
            match request {
                HelperRequest::OpenEditor => {
                    // A plugin allows one editor view at a time: release the
                    // previous one before asking for another.
                    editor = None;
                    match backend.open_editor() {
                        Ok(window) => {
                            #[cfg(target_os = "macos")]
                            appkit::activate();
                            editor = Some(window);
                            #[cfg(target_os = "macos")]
                            {
                                editor_opened_at = Some(std::time::Instant::now());
                            }
                            send_event(&mut out, &HelperEvent::EditorOpened)?;
                        }
                        Err(err) => send_error(&mut out, format!("{err:#}"))?,
                    }
                }
                HelperRequest::CloseEditor => {
                    editor = None;
                    send_event(&mut out, &HelperEvent::EditorClosed)?;
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

fn send_event(out: &mut protocol_out::Out, event: &HelperEvent) -> Result<()> {
    protocol_out::send(out, &serde_json::to_vec(event)?)?;
    Ok(())
}

fn send_error(out: &mut protocol_out::Out, message: String) -> Result<()> {
    eprintln!("helper error: {message}");
    send_event(out, &HelperEvent::Error { message })
}

/// Let the OS deliver window events, which plugin editors depend on.
#[cfg(windows)]
fn pump_messages() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// AppKit only shows and draws windows for a process that is a launched
/// `NSApplication` and drains its event queue on the main thread. Without this
/// a plugin editor window is created but never appears, and the Dock icon does
/// nothing when clicked.
#[cfg(target_os = "macos")]
mod appkit {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSEventMask};
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

    /// Make this process a regular app. Call once, on the main thread, before
    /// any plugin is loaded.
    pub fn init() {
        let Some(mtm) = MainThreadMarker::new() else {
            eprintln!("helper: not on the main thread; editor windows will not show");
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        // Regular, not Accessory: the editor window should be reachable from
        // the Dock and Cmd-Tab like any app window.
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        app.finishLaunching();
    }

    /// Bring this process to the front, so a newly opened editor is not hidden
    /// behind the Surf window that asked for it.
    pub fn activate() {
        if let Some(mtm) = MainThreadMarker::new() {
            #[allow(deprecated)] // `activate` needs macOS 14; this works on all versions.
            NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        }
    }

    /// Test hook: press the close button of every visible window, exactly as a
    /// user clicking the title bar would (`performClose:`). Used by the
    /// `editor_reopen` probe through `SURF_TEST_CLOSE_EDITOR_AFTER_MS`.
    pub fn perform_close_all() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        for window in NSApplication::sharedApplication(mtm).windows().iter() {
            if window.isVisible() {
                window.performClose(None);
            }
        }
    }

    /// Dispatch every pending event without blocking.
    pub fn pump() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let app = NSApplication::sharedApplication(mtm);
        let now = NSDate::distantPast();
        loop {
            // SAFETY: called on the main thread with a valid mode and date.
            let event = unsafe {
                app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&now),
                    NSDefaultRunLoopMode,
                    true,
                )
            };
            let Some(event) = event else { break };
            app.sendEvent(&event);
        }
        app.updateWindows();
    }
}

#[cfg(target_os = "macos")]
fn pump_messages() {
    appkit::pump();
}

/// Linux editors (X11 + `IRunLoop`) are not wired up yet.
#[cfg(all(unix, not(target_os = "macos")))]
fn pump_messages() {}

/// Render audio into the shared ring, keeping it topped up to a fixed cushion.
///
/// The consumer's drain rate sets the pace, so this needs no clock of its own.
fn audio_loop(mut instrument: Box<dyn Instrument>, map: Arc<SharedMap>, stop: Arc<AtomicBool>) {
    // Ask for audio-class scheduling before sizing the cushion: the whole point is
    // that a reliably-woken thread needs far less queued audio to stay glitch-free.
    // On Windows this also sets a 1 ms timer resolution so `sleep(1ms)` means ~1 ms;
    // the default ~15.6 ms granularity would let the ring run dry.
    let _rt = surf_platform::rt::promote_audio_thread(Duration::from_millis(1));

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
