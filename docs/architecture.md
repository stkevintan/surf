# Surf Architecture

> Top-down description of Surf, a desktop MIDI studio: live MIDI, a falling-note visualizer, recording, piano-roll editing, VST hosting, and MIDI/WAV export.
>
> - **L1** describes the system: processes, layers, threads, and the rules the rest depends on.
> - **L2** describes each module: its job, API, and implementation.
> - **L3** describes feature details: algorithms, data layouts, protocols, timing.
>
> Each section separates **Current** (what the code does today) from **Target** (where it is heading). Phase 1, the workspace split and platform layer, is done; see §1.7 and the roadmap at the end. The UI design lives in [design/README.md](design/README.md); this document covers everything below the pixels.

---

## L1 — System

### 1.1 Context

```
            ┌──────────────┐   MIDI    ┌───────────────────────────────┐
 Keyboard ─▶│ OS MIDI API  │──────────▶│                               │
            └──────────────┘           │            Surf               │──▶ .mid / .wav files
            ┌──────────────┐  audio    │  (UI process + plugin hosts)  │
 Speakers ◀─│ OS audio API │◀──────────│                               │◀── .mid files, plugin state
            └──────────────┘           └──────────────┬────────────────┘
                                                      │ loads (in an isolated process)
                                       ┌──────────────▼────────────────┐
                                       │ Third-party VST3 / VST2 / CLAP │
                                       │ (Kontakt, Pianoteq, …)         │
                                       └────────────────────────────────┘
```

Surf talks to three external systems: the OS MIDI and audio APIs, the file system (MIDI files, WAV export, plugin state blobs), and third-party plugin binaries. Plugins are the main stability risk and never run in the UI process.

### 1.2 Process model

```
surf (UI process)                                        surf-plugin-host  (one per plugin instance)
┌─────────────────────────────────────────────┐          ┌──────────────────────────────────────┐
│ main thread    GPUI event loop + views       │  stdin   │ main thread   control loop, OS        │
│ audio thread   cpal callback → engine Core   │  JSON ─▶ │               message pump, editor    │
│ MIDI callback  OS MIDI driver thread         │  ◀─ JSON │ audio thread  MIDI ring → plugin →    │
│ workers        offline render, file I/O      │  stdout  │               audio ring (MMCSS)      │
└──────────────────────┬──────────────────────┘          │ control thread stdin reader           │
                       │  shared memory: header + audio ring + MIDI ring           └────────────┬─────────────────────────┘
                       └─────────────────────────────────────────────────────────────────────┘
```

- **The UI process** owns the project, the views, the audio device, MIDI input, and the built-in instruments.
- **A plugin host** owns exactly one plugin instance, its native editor window, and its audio thread. If a plugin crashes, it only takes its host down; the UI swaps in the built-in piano and keeps running.
- **Control plane:** line-delimited JSON over the host's stdin/stdout. It carries requests and events: load state, open editor, render.
- **Data plane:** one shared-memory region per host with two lock-free single-producer/single-consumer (SPSC) rings, one for audio and one for MIDI. The real-time paths never touch the control pipe.

*Current:* one host per distinct plugin. Tracks that pick the same plugin share its host and are told apart by MIDI channel, so one Kontakt can play several tracks in multi mode (§2.6). Plugin loading is verified on Windows and macOS (Kontakt 8 VST3). *Target:* the same on Linux.

### 1.3 Layers

```
 ┌──────────────────────────────────────────────────────────────┐
 │ apps/surf          wiring, window, startup                   │
 ├──────────────────────────────────────────────────────────────┤
 │ surf-ui            GPUI views, theme, UI state (selection)   │  presentation
 ├──────────────────────────────────────────────────────────────┤
 │ surf-core          Project model, commands, undo, tempo map  │  domain (pure)
 ├────────────────┬───────────────────┬─────────────────────────┤
 │ surf-midi      │ surf-audio        │ surf-plugin             │  engines
 │ MIDI I/O, SMF  │ engine, synth,    │ host client, protocol,  │
 │                │ offline render    │ shared memory, backends │
 ├────────────────┴───────────────────┴─────────────────────────┤
 │ surf-platform      OS seams: shm, RT priority, dirs, windows │  platform
 └──────────────────────────────────────────────────────────────┘
   surf-plugin-host (binary)      surf-visual (wgpu stage renderer, later)
```

Dependencies only point downward. `surf-core` has no I/O and no platform code, so all editing logic is unit-testable. Every engine crate depends on `surf-core` types (`Note`, `TempoMap`), never on `surf-ui`.

### 1.4 Threads and real-time rules

| Thread | Owner | May block? | May allocate? | Talks to others via |
|---|---|---|---|---|
| UI main | GPUI | yes (briefly) | yes | `Command` channel → audio; atomics ← audio |
| Audio callback | cpal | **no** | **no** (except `Command::Play`, which moves a `Timeline` in) | `try_lock` on Core, `try_recv` commands, atomics out |
| MIDI driver callback | OS | no | no | `Command` channel → audio; timed events → UI |
| Host audio | host binary | sleeps 1 ms between top-ups | no | SPSC rings |
| Host control | host binary | yes | yes | JSON pipe, `mpsc` to host main |
| Render worker | UI process | yes | yes | `mpsc` result → UI |

Invariants:

1. **The audio thread is the only clock.** The playhead is a frame counter advanced by the audio callback, or by a silent timer thread when there is no device. The UI reads it from an atomic and never computes time on its own.
2. **MIDI bypasses the UI.** The OS callback sends note commands straight to the audio thread, so latency does not depend on frame rate. A timestamped copy goes to the UI for drawing and recording.
3. **No plugin code in the UI process.** Scanning only walks the file system; loading happens in a host.
4. **Lock-free in real time.** The audio callback uses `try_lock`; if it fails, it outputs silence for that block rather than wait. Shared-memory rings are SPSC with acquire/release atomics.

### 1.5 Data model (summary)

```
Project ─┬─ TempoMap (tempo events, time signatures)        surf-core, persisted
         └─ Track* ─┬─ Instrument { kind: Builtin | Plugin(path, state blob) }
                    ├─ Mixer { volume dB, pan, mute, solo, arm }
                    └─ Region* { start, length (ticks), name } ─ Note* { key, vel, start, length (ticks) }

UiState ─ selection (regions | notes), focus area, edit region, zoom/scroll   surf-ui, not persisted
```

Positions are stored in **ticks** (PPQ 960 in the target; 480 today) and converted to seconds or frames through the tempo map only at the edges: playback scheduling, rendering, drawing.

*Current:* `surf-core::project::Project` holds tracks → regions → notes, in ticks at the file's PPQ (480 for files Surf writes). Note starts are relative to their region, so moving a region moves its notes. Every object has a stable `Id`, so selection and undo survive edits. Mixer fields are `volume`, `mute`, `solo` and `channel`, and `instrument` names the plugin the track plays (`None` is the built-in piano). Changing it is an undoable edit like any other (§2.6). `Song` remains the MIDI-file form; `Project::from_song` makes one track per MIDI channel, and `Project::to_song` / `timeline()` flatten it back for export and playback.

### 1.6 Cross-platform strategy

Platform differences are pushed down to a few narrow traits; everything above them is shared code.

| Concern | Windows | macOS | Linux |
|---|---|---|---|
| Audio out | cpal (WASAPI/ASIO) | cpal (CoreAudio) | cpal (ALSA/PipeWire via ALSA) |
| MIDI in | WinMM | CoreMIDI (`coremidi` 0.8) | ALSA sequencer (`alsa` 0.11) |
| Shared memory | `CreateFileMapping` | `shm_open` + `mmap` | `shm_open` + `mmap` |
| RT priority (host audio) | MMCSS "Pro Audio" + `timeBeginPeriod(1)` | `thread_policy_set` (time-constraint) | `SCHED_FIFO` via rtkit (optional) |
| Plugin formats | VST3, VST2 (`.dll`) | VST3 (editor works), VST2 (`.vst` bundle, no editor yet) | VST3, VST2 (`.so`); no editors yet |
| Editor event loop | Win32 message pump | the host becomes a regular `NSApplication` (Dock icon) and drains AppKit events every 2 ms on the main thread | not yet (`X11` + `IRunLoop`) |
| Format rules | `surf-plugin::format` maps a path to a format and, for a macOS bundle, to its Mach-O binary (via `Info.plist`) | | |
| Editor parent window | HWND | NSView | X11 window |
| Data dir | `%APPDATA%\Surf` | `~/Library/Application Support/Surf` | `$XDG_DATA_HOME/surf` |
| Scan roots | `Common Files\VST3`, `VSTPlugins` | `/Library/Audio/Plug-Ins/VST3`, `~/Library/…` | `~/.vst3`, `/usr/lib/vst3` |

Why not `midir`: midir 0.11 needs `coremidi` 0.9, which needs `core-foundation` ≥ 0.10.1, but GPUI 0.2.2 pins `core-foundation = "=0.10.0"`; midir 0.10 links an older `alsa` than cpal, and Cargo allows only one crate per `links = "alsa"`. Calling the three OS APIs directly avoids both conflicts and is about 300 lines.

macOS build: GPUI is built with `runtime_shaders`, so its Metal shaders compile at startup and the build needs only the Command Line Tools, not full Xcode.

Main-thread constraint: on macOS, GPUI and any second windowing stack (winit/Bevy for `surf-visual`) both need the main thread. A full-screen stage renderer must therefore be its own process, which is the same pattern as plugin hosts.

### 1.7 Repository layout

*Current (after Phase 1)*

```
src/main.rs                the app binary (`surf`), plus a lib facade for examples/
crates/
  surf-core/               song model, tempo map, recorder (pure: no I/O, no deps)
  surf-midi/               live input (WinMM / CoreMIDI / ALSA seq), SMF read/write
  surf-audio/              engine, Instrument trait, built-in synth, offline render
  surf-platform/           SharedRegion, promote_audio_thread, data_dir
  surf-plugin/             protocol, client, remote, shm rings, scan;
                           backends/{vst3, vst2 (Windows)} behind feature `host`
  surf-ui/                 GPUI app (still the original single view, app.rs)
apps/surf-plugin-host/     the `surf-vst-host` binary
vendor/vst3-host, vendor/vst   patched forks (see docs/vendor-patches.md)
examples/                  probes and smoke tests; Windows-only probes compile to a stub elsewhere
```

Dependency graph (normal dependencies only):

```
surf (app) ──▶ surf-ui ──▶ surf-plugin ──▶ surf-audio ──▶ surf-core
                  │             └────────▶ surf-platform
                  ├──▶ surf-midi ──▶ surf-core
                  └──▶ surf-audio, surf-platform, surf-core
surf-plugin-host ──▶ surf-plugin[host] (+ vst3-host, vst on Windows)
```

The app binary does not link `vst3-host` or `vst`; only the host does (`cargo tree -p surf` shows neither).

*Still to come*

```
crates/surf-audio/         engine with instrument slots, routes and slot gains (phase 3 ✅)
crates/surf-visual/        wgpu stage renderer (phase 4)
```

Where the original files went:

| Before | Now |
|---|---|
| `song.rs` (model, tempo) | `surf-core::song` |
| `song.rs` (SMF parse/write) | `surf-midi::smf` (`load`, `save`, `from_bytes`, `to_bytes`) |
| `recorder.rs` | `surf-core::recorder` |
| `midi_input.rs` | `surf-midi::input` (`MidiHub`, backends `winmm` / `coremidi_backend` / `alsa_seq`) |
| `audio/mod.rs`, `engine.rs`, `synth.rs`, `render.rs` | `surf-audio` |
| `audio/ipc.rs` | `surf-plugin::shm`, built on `surf-platform::shm::SharedRegion` |
| `audio/vst_client.rs` | `surf-plugin::client` + `surf-plugin::protocol` |
| `audio/remote.rs` | `surf-plugin::remote` |
| `audio/vst.rs`, `vst2.rs` | `surf-plugin::backends::{vst3, vst2}` |
| `audio/vst_scan.rs` | `surf-plugin::scan` |
| `paths.rs` | `surf-platform::dirs` |
| `bin/vst_helper.rs` | `apps/surf-plugin-host` (MMCSS code moved to `surf-platform::rt`) |
| `app.rs` | `surf-ui::{session, shell, views/*}` |

---

## L2 — Modules

Each module is listed with its responsibility, public surface, and implementation notes.

### 2.1 `surf-core` — project model and edits

**Responsibility:** the single source of truth for musical data, and every operation that changes it.

- `project.rs`: `Project { tempos, time_signature, tracks: Vec<Track> }`, `Track { channel, mute, solo, volume, regions }`, `Region { start, len, notes }`, `Note { key, velocity, start, len }`, all with `Id`s. Conversions: `from_song`, `to_song`, `timeline()`. `timeline()` drops tracks that are muted or soloed out and tags every note with its track's index (`TimedNote::track`), which the engine routes. Volume is the engine's gain stage, so velocities play as written. Notes past a region's end are kept but don't play.
- `edit.rs`: `Edit`, an enum covering every change:
  - notes: move, resize, velocity, add, delete, quantize;
  - regions: move (across tracks), resize, split, join, insert, remove;
  - tracks: add, remove, set field;
  - `Batch` for compound steps.

  `Edit::apply(&mut Project) -> Edit` returns the exact inverse. Clamped moves restore the original notes rather than applying the opposite offset.
- `History`: undo/redo stacks of labelled steps. A drag calls `preview()` on each pointer move, which replaces the previous preview instead of stacking a new step, and `commit()` on pointer-up, so one gesture is one ⌘Z. `cancel()` (Esc) restores the state before the drag.
- `recorder.rs`: live `(event, Instant)` pairs → notes, independent of the audio clock. The UI turns a finished take into a new `Take N` region on the record-armed track.

Tests apply every edit kind and undo it, asserting the project is bit-for-bit unchanged. They also cover: split then join restoring the notes, partial-strength quantize, a 10-step drag undoing in one step, and cancel.

### 2.2 `surf-midi` — MIDI I/O and files

**Responsibility:** get MIDI in from devices, and read and write Standard MIDI Files.

**Input.**
- *Current:* `MidiHub` (`midi_input.rs`) enumerates WinMM devices and opens one with `midiInOpen` and a C callback. The callback parses status bytes (`parse_message`: note on/off, CC 64), sends a `Command` to the audio engine, and sends `TimedMidi { at: Instant, event }` to the UI channel. `drain()` is polled by the UI once per frame.
- *Hot-plug:* `MidiHub::poll()` runs once per UI frame. It re-enumerates ports when the backend reports a change; CoreMIDI notifies, and WinMM and ALSA fall back to a 2 s timer. It reconnects to the same port by name, auto-connects a new port when nothing is connected, and returns a `PortChange` for the status bar.
- *Now:* `MidiHub::new(sink: NoteSink)`. The sink is a closure called on the driver thread; the app points it at the audio engine. `surf-midi` therefore does not depend on `surf-audio`. Each OS backend exposes `list_ports()` and `open(index, deliver)`, and shares one `parse_message`. The CoreMIDI backend also walks multi-message packets and running status, and skips SysEx and real-time bytes.

**Files** (`song.rs` today):
- Import with `midly`: accept metrical or SMPTE timing (SMPTE falls back to the default PPQ), read tempo and time-signature meta events, pair note on/off per `(channel, key)` with a stack (an overlapping same-key note-off closes the most recent note-on), drop drum channel 10 and keys above 127, then extend durations under sustain (L3 §3.6).
- Export as a type-1 SMF: a conductor track (tempo map + time signature), then the notes, each on its track's MIDI channel (`Project::to_song`). Re-importing gives the same tracks back, one per channel.

### 2.3 `surf-audio` — engine, instruments, render

**Responsibility:** produce audio in real time and offline from scheduled notes.

```rust
pub trait Instrument: Send {          // surf-audio/src/lib.rs
    fn note_on(&mut self, channel: u8, key: u8, velocity: u8);
    fn note_off(&mut self, channel: u8, key: u8);
    fn all_notes_off(&mut self);
    fn set_sustain(&mut self, channel: u8, down: bool);
    fn control_change(&mut self, _channel: u8, _cc: u8, _value: u8) {}
    fn process(&mut self, out: &mut [f32], frames: usize); // interleaved stereo
    fn latency_samples(&self) -> u32 { 0 }
    fn set_sample_rate(&mut self, _sr: f32) {}
    fn name(&self) -> &str;
}
```

`Instrument` is the seam that lets the engine stay unaware of where sound comes from. Implementations: `Synth` (built-in, ignores the channel), `RemoteInstrument` (a plugin in a host, via shared memory), and inside the host binary `VstInstrument` / `Vst2Instrument`, which put the channel on the MIDI they send.

**Engine** (`engine.rs`):
- `AudioEngine::new()` opens the default cpal output with every instrument slot empty. If no output is available, it starts a `NullClock` thread that renders 240-frame blocks every 5 ms, so the timeline keeps moving without a device.
- **Slots and routes.** The engine mixes up to `MAX_SLOTS` (32) instruments. `SetSlot { slot, instrument, ack }` fills or empties one; the old instrument is dropped on the audio thread before the ack fires, so the UI can then release the last plugin handle. `SetRoutes(Vec<Route>)` says, by track index, which slot and MIDI channel each track plays: `Route { slot, channel }`. `SetSlotGain` sets a slot's gain. `SetInput(track)` picks the track live input plays.
- The UI sends `Command`s over an unbounded crossbeam channel; the callback drains them at the start of each buffer.
- `Core::render(frames)` works in 64-frame chunks. For each chunk it triggers note-offs, then note-ons whose start frame falls inside it, each on its track's route. Then it runs **every** filled slot, even a silent one, since a plugin host's audio ring has to keep draining. It scales each slot by its gain, sums the slots, applies master volume and clamping, and tracks the peaks.
- Live notes remember the route they went to, so a note-off follows its note-on even if the input track changed while the key was down.
- **Edits during playback.** `Update { timeline }` swaps in an edited timeline without restarting. The position stays. A sounding note that is still in the new timeline (same track by stable id, so adding or deleting other tracks doesn't disturb it; same key; still covering now) keeps sounding, with its end moved to the new end. One that was removed or changed stops. While playing, an edited note that now covers the playhead but wasn't sounding (dragged back under it, or drawn there) starts. Notes ahead of the playhead play from the new timeline. The session sends `Update` after every edit, and on every move of a drag, so a dragged note is heard where it is. The swapped-out timeline goes back to the UI thread over a bounded channel to be freed. If that channel is full, the core holds it in a pre-reserved list and sends it on a later update, so the callback doesn't free a timeline.
- Status goes out through atomics: `position_frames`, `duration_frames`, `playing`, `peak`, `slot_peaks[]`, `sample_rate`.

**Target changes:**
- Pan per slot, and effects inserts.
- Diff the timeline instead of rebuilding it whole, once projects get large enough for the rebuild to show up per drag move.

**Output changes:** the cpal error callback classifies stream errors into `OutputEvent`s instead of printing them:
- `Rerouted { device }`: the default output changed and cpal followed it, for example a USB MIDI keyboard that is also an audio interface;
- `Lost`: the output device disappeared;
- `Glitch`: an underrun, counted in `EngineStatus::glitches`;
- `Error`: anything else.

The UI drains these each frame and shows them in the status bar.

**Offline render** (`render.rs`): `render_with_instrument(timeline, &mut dyn Instrument, path, opts)` runs the same chunked trigger loop with no device and writes 24-bit stereo WAV via `hound`. It handles plugin latency and the tail (L3 §3.8). Export renders one stem per slot and `mix_stems` sums them with the slot gains and master volume. Built-in stems render on a UI-process worker thread; plugin stems render inside their host.

### 2.4 `surf-plugin` — hosting client, protocol, transport

**Responsibility:** discover plugins, start and supervise hosts, and expose each host as an `Instrument` plus a control handle.

| Part | Current file | Role |
|---|---|---|
| `scan` | `vst_scan.rs` | Walk the standard roots (+ `SURF_VST_SCAN`) recursively, up to depth 6, for `.vst3` and this OS's VST2 type (from VST2 roots only). Never loads anything. Max 400 results, sorted, deduplicated. |
| `format` | — | Format detection by extension, VST2 bundle → binary resolution, and `Format::has_editor`. |
| `shm` | `ipc.rs` | Shared region layout and SPSC rings (L3 §3.3). |
| `protocol` | `vst_client.rs` (`HelperRequest`/`HelperEvent`) | serde-tagged JSON enums (L3 §3.4). |
| `client` | `vst_client.rs` (`VstClient`) | Spawn the host, handshake, request/response with timeouts, health check, log capture. |
| `remote` | `remote.rs` (`RemoteInstrument`) | `Instrument` impl: MIDI → ring push; `process` → ring pop + backlog trim. |
| `backends` | `vst.rs`, `vst2.rs` | Format adapters, linked only into the host binary. |

**Client lifecycle** (`VstClient::load`):
1. Create the shared map under a unique name (`Local\SurfVst-<pid>-<nanos>` on Windows) and write the sample rate.
2. Spawn the host with `--shm --plugin --sample-rate [--state]`. Set the working directory to the plugin's parent folder, give it a private empty `TEMP`, and send stderr to an append-only log file (L3 §3.10).
3. Wait up to **240 s** for `Ready { name, sample_rate }`. Kontakt takes about 80 s on a cold cache.
4. On failure: get the exit code, kill the child, and replay this run's part of the log to stderr.
5. While running: the UI polls `is_alive()` (child exit + `helper_alive` flag). On death, the plugin is marked offline and its tracks fall back to built-in pianos until the user picks it again.
6. On drop: send `Shutdown`, clear `main_alive`, and wait.

**Target:** non-blocking requests (render and state save return futures or poll handles instead of blocking the UI thread); and a CLAP backend via `clack`.

### 2.5 `surf-plugin-host` — the host binary *(today `src/bin/vst_helper.rs`)*

Startup order matters, because each step fixes a real Kontakt failure:

1. Install a panic hook that writes `surf-vst-host-panic.log`.
2. Initialize COM (STA) on Windows. The guard outlives the plugin.
3. Open the shared map **before** loading the plugin.
4. `chdir` + `SetDllDirectoryW` to the plugin's parent folder (opt out with `SURF_NO_CHDIR`).
5. Pick the backend with `format::detect`: this OS's VST2 type goes to the VST2 backend, everything else to VST3. Load it with the optional state blob.
6. Set `helper_alive`, spawn the audio thread, send `Ready`.
7. Spawn the control thread (reads stdin lines into an `mpsc` channel). When stdin closes, it sends `Shutdown`.
8. Main loop, every 2 ms: pump OS messages (the editor window needs it), idle the VST2 editor, handle requests, exit if `main_alive` is false.

The host audio thread is described in L3 §3.2.

### 2.6 `surf-ui` — views *(today `app.rs`)*

**Responsibility:** draw the project, turn input into `surf-core` commands, and keep UI-only state.

- *Current:* one `SurfApp` GPUI entity. Each frame, `render()` polls `drain_midi`, `poll_vst_health`, and `poll_render`, trims old live notes, reads `EngineStatus`, lays out 88 keys, and draws the top bar, transport, sidebar, visualizer, keyboard, and status bar. Modes: `Live` / `Play`.
- *Target:* the Logic-style shell specified in [design/README.md](design/README.md): control bar, toolbar, Inspector, Library, Tracks / Perform, a bottom editor with Mixer / Piano Roll / Event List, and a status bar.

| Target view | Drawing | Owns |
|---|---|---|
| `shell` | GPUI elements | pane visibility, splitter height, shortcuts |
| `tracks` | `canvas` | track-area zoom/scroll, region drag state |
| `roll` | `canvas` | roll zoom/scroll, key range, note drag state |
| `event_list` | GPUI list | — |
| `mixer`, `inspector` | GPUI elements | strip widgets (fader, knob, meter) |
| `library` | GPUI list | scan results, search query |
| `perform` | `canvas` *(later: `surf-visual`)* | look-ahead window |
| `theme` | — | constants generated from `docs/design/tokens.json` |

`UiState { selection: Regions(Set) | Notes(Set), focus: Tracks | Roll, edit_region, … }` lives in `surf-ui`, not in the project file.

**Instruments per track** (`session.rs`, `rack.rs`). The Library puts the picked plugin on the selected track as an undoable edit. After every edit, `Session::sync_instruments` makes the engine match the project:
- every track on the built-in piano gets its own piano in its own slot, so its fader is a real gain stage;
- tracks that name the same plugin share one host and one slot, each on its own MIDI channel (Kontakt multi);
- hosts no track uses are stopped, and their state is parked for undo (§3.9).

`rack::plan` is the pure part: it maps each track to a `Route` and each slot to a gain. A slot with one track takes that track's fader. A shared plugin's audio comes back already mixed, so its tracks are balanced with CC 7 (channel volume) instead. If a plugin fails to load, nothing changes and nothing goes on the undo stack. Slot 0 is a spare piano for tracks that find no free slot. Live input plays the record-armed track, else the selected one. Opening a MIDI file keeps the loaded plugins: each new track takes the instrument of the old track on its channel.

### 2.7 `surf-platform` — OS seams

```rust
pub trait SharedRegion: Send + Sync { fn create(name: &str, size: usize) -> Result<Self>; fn open(..); fn as_ptr(&self) -> *mut u8; }
pub fn promote_audio_thread() -> RtGuard;           // MMCSS / thread_policy / SCHED_FIFO
pub fn data_dir() -> PathBuf;                        // current: paths.rs
pub fn plugin_roots() -> Vec<ScanRoot>;
pub fn editor_parent(window: &impl HasWindowHandle) -> RawWindowHandle;
```

`data_dir()` today: `SURF_STATE_DIR` → `%APPDATA%\Surf` → `<repo>/.surf` (when `Cargo.toml` is in the cwd) → `<exe dir>/surf-state` → `<temp>/surf-state`. Each candidate is probed for writability once and the result is cached in a `OnceLock`.

### 2.8 `surf-visual` — stage renderer *(phase 4)*

A pure wgpu renderer exposing `render(&SceneState, &TextureView)`. It runs as a separate process for the full-screen stage window (it reads the playhead from shared memory, like plugin hosts), headless for MP4 export, and optionally as a texture shown inside GPUI. It gives the effects GPUI cannot do: additive blending, bloom, GPU particles. The particle rules (spawn, physics, fade) are plain data shared with the CPU fallback.

---

## L3 — Feature details

### 3.1 Timing and transport

- **Units.** Project positions are ticks. The engine works in frames at the device rate (typically 48 kHz). Seconds appear only at boundaries: the LCD, `Timeline` today.
- **Play.** `Command::Play { timeline, from }` sets `pos_frames = from × sr`, finds `next_note` by binary search (`partition_point(start < from)`), clears active notes, and marks playing. `Seek` does the same without changing play state.
- **Triggering a chunk `[start, end)`:** first, note-offs for active notes with `end_frame ≤ end`, so back-to-back notes on the same key retrigger cleanly; then note-ons for notes whose start frame is in the chunk. A note with start < chunk start is skipped (already past). Note end is at least start + 1 frame.
- **End and loop.** When `end ≥ duration_frames`: if looping, reset to 0 (target: to the cycle start, and no wrap while recording); otherwise stop and send all-notes-off.
- **Resolution.** Scheduling is quantized to 64-frame chunks (1.33 ms at 48 kHz) for built-in instruments. For plugins, see §3.2.

### 3.2 Real-time audio path with a plugin

```
UI audio callback (cpal, ~480 frames)          host audio thread (MMCSS, 1 ms sleep)
  Core.render → RemoteInstrument.process        loop:
    trim_audio_backlog(1536)                      drain MIDI ring → plugin.note_on/off…
    pop_audio(frames) → zero-fill on underrun     need = 512 − audio_backlog
  RemoteInstrument.note_on → push_midi             while need ≥ 64: plugin.process(64) → push_audio
                                                  sleep(1 ms)
```

- **Cushion.** The host keeps the ring topped up to **512 frames** (10.7 ms at 48 kHz), which is one 480-frame device buffer plus a margin. Override with `SURF_AUDIO_CUSHION` (128–16384). It was measured at 0 underruns over 800 buffers with MMCSS + `timeBeginPeriod(1)`.
- **Self-pacing.** The host renders "up to the cushion" instead of "since elapsed time". Rendering whole 64-frame blocks against a time target loses a remainder every pass and drifts slower than real time. Topping up to a depth corrects itself at the consumer's drain rate.
- **Backlog trim.** If the consumer stalls (a long GUI operation), the UI drops audio beyond 1536 frames (32 ms) so latency cannot grow without bound.
- **Underrun.** It plays silence and never repeats stale audio.
- **Known gap.** `MidiMsg` carries no frame offset; the host applies MIDI at the start of its next 64-frame block, giving roughly 1–11 ms of jitter on plugin playback. *Target:* add a target frame index to `MidiMsg` and split the plugin block at event offsets.

### 3.3 Shared-memory layout (`surf-plugin::shm`)

```
offset  size   field
0       8      magic  0x5355_5246_5653_5431 ("SURFVST1")
8       4      sample_rate
12      4      channels (2)
16      8      audio_write   (frames, monotonic)   — host advances
24      8      audio_read    (frames, monotonic)   — UI advances
32      8      midi_write    (messages, monotonic) — UI advances
40      8      midi_read     (messages, monotonic) — host advances
48      4      helper_alive
52      4      main_alive
64      16384 × 2 × f32      audio ring  (~341 ms @ 48 kHz)
…       1024 × 16 B          MIDI ring   MidiMsg { kind, a, b, channel } (u32 each)
```

- Indices only increase; the slot is `index % capacity`. Fill level is `write − read`. The producer only writes `write`, the consumer only writes `read`, both with release/acquire ordering.
- `push_audio` fails (and the host drops the block) when the ring is full; `push_midi` fails when 1024 messages are pending.
- `MidiKind`: 1 NoteOn(a=key, b=vel), 2 NoteOff(a=key), 3 Sustain(a=down), 4 AllNotesOff, 5 PitchBend (reserved), 6 ControlChange(a=controller, b=value). `channel` is the MIDI channel 0–15; it used to be padding, so an older UI plays everything on channel 1.

### 3.4 Control protocol (stdin/stdout JSON)

One JSON object per line, tagged with serde (`"cmd"` for requests, `"event"` for events). Before loading a plugin, the host moves the protocol to a private copy of its stdout pipe and points its own stdout (including the C runtime's) at stderr, which is the host log. A plugin that prints to stdout can no longer join or corrupt a protocol line.

| Request (`cmd`) | Fields | Reply (`event`) |
|---|---|---|
| `OpenEditor` | — | `EditorOpened` \| `Error` |
| `CloseEditor` | — | `EditorClosed` |
| `SaveState` | `path` | `StateSaved { bytes, path }` \| `Error` |
| `LoadState` | `path` | `StateLoaded` \| `Error` |
| `Render` | `output, sample_rate, notes: [RenderNote], duration, tail_secs, controls: [(channel, cc, value)]` | `RenderDone { path, duration, peak }` \| `Error` |
| `Ping` | — | `Pong` |
| `Shutdown` | — | (process exits) |

The host sends `Ready { name, sample_rate }` once, unprompted. `Exited` is synthesized by the client's stdout reader on EOF. `RenderNote { key, velocity, channel, start, end }` is in seconds.

*Target additions:* `RenderProgress { frac }` for async export, `SetParam`/`ParamChanged` for automation, and a protocol version number in `Ready`.

### 3.5 Live input, recording, and visual feedback

1. The OS MIDI callback parses the message. NoteOn with velocity 0 counts as NoteOff; CC 64 ≥ 64 means pedal down.
2. It sends `Command::NoteOn` to the audio channel immediately (sound), and `TimedMidi { at: Instant::now() }` to the UI channel.
3. The UI drains the channel once per frame. It appends a `LiveNote { start: at }` for the rising-note visual, marks keys held by the pedal for the keyboard highlight, and feeds the `Recorder` while recording.
4. The `Recorder` stores note starts relative to the recording start. Note-offs that arrive while the pedal is down are deferred until pedal-up; a retrigger without a note-off closes the previous note. Minimum length is 10 ms. `finish(now)` closes open notes and returns `RecordedNote`s.
5. `Song::from_recorded` converts seconds to ticks at 120 BPM / PPQ 480. *Target:* the recording becomes a `Take N` region on the armed track, positioned by the transport's bar at record start, and converted through the project tempo map.

The computer keyboard (`A W S E D F T G Y H U J K O L P ;` → C4–E5 today, C3–E4 in Logic octave naming in the target) goes through the same `handle_live` path with `send_audio = true`.

### 3.6 MIDI file import details

- Notes on the same `(channel, key)` are matched last-in first-out (a stack). A note-on with no matching note-off is dropped. Duration is at least 1 tick.
- Sustain: for each note, find the last pedal event at or before its note-off. If the pedal is down, extend the note to the next pedal-up on that channel.
- The tempo map always has an entry at tick 0 (default 120 BPM). Tick ↔ seconds integrates over tempo segments.
- Channel 10 (drums) is excluded from the piano visualization.

### 3.7 Built-in synth (`synth.rs`)

- Additive voice with 6 partials at ratios `1, 2.003, 3.009, 4.021, 5.041, 6.069` (slight inharmonicity) and gains `1, .42, .21, .11, .06, .035`.
- The start phase is seeded from key and velocity so stacked notes don't phase-cancel.
- Decay time constant `τ = clamp(3.8 · (261.63 / f)^0.35, 0.9, 9.0)` s, so low notes ring longer.
- Pan spreads keys across ±0.35. The velocity curve is `(v/127)^1.4`.
- Up to 64 voices; when full, the oldest voice is stolen. Retriggering a key releases its existing voice first. On note-off with the pedal down a voice is marked `held = false` but keeps ringing; pedal-up releases every voice whose key is already up.

### 3.8 Offline rendering

- **Stems:** export renders each instrument slot on its own: built-in pianos with `render_timeline_to_wav` on a worker thread, plugins in their host. `mix_stems` then sums them with each slot's gain and the master volume; the UI polls `render_rx`.
- **Plugin:** the UI sends `Render` to the host. The host saves the live instance's state, creates a **fresh** instance, sets `ProcessMode::Offline`, loads the state, then runs the render loop.
- **Loop:** 64-frame blocks. Total frames = `(duration + tail) × sr + latency`. The first `latency_samples()` frames are discarded, so plugin delay doesn't shift the file. Samples are clamped to ±1 and written as 24-bit integers (`× 8 388 607`), stereo. The default tail is 3 s.
- **Known gaps:** the plugin render blocks the UI thread (`VstClient::render` waits synchronously), and it doesn't pump messages, so the editor freezes too. Creating a second instance doubles memory and load time, which is about 80 s for Kontakt. *Target:* async with progress events, and optionally reuse the live instance (pause live output, switch to offline mode, render, switch back).

### 3.9 Plugin state

- State is an opaque blob from `getState` (VST3: component + controller; VST2: chunk). Path: `data_dir()/vst-<hash>.state`, where the hash is a 64-bit `DefaultHasher` of the plugin path. Moving the plugin changes the hash.
- On load, the state is applied if the file exists and `SURF_NO_STATE` is not set.
- When the last track lets a plugin go, Surf parks its state in a temp file and stops the host. Undo starts it again from that state, so undoing a change of instrument brings Kontakt back as it was.
- Kontakt has no "load NKI" API. The supported workflow is: load the instrument in Kontakt's own editor, save state from Surf, and restore it on the next load or render.
- **Project file.** `surf-core::document` saves a `.surf` file: JSON with `format`, `version`, the `Project`, and one base64 state blob per plugin path. Tracks that share a plugin share its blob. Saving asks each running host for its state. Plugins that aren't running (offline, or failed to load) keep the blob they were opened with. Opening restarts every host from the file's blobs (through the parked-state path), then `Project::repair` moves the id counter past every id in use. Writes go to a temp file and are renamed into place. A newer `version` is refused.
- The hash-named files are the "default sound" for a plugin in a new project.
- `History` tracks the saved point by step serial, so undoing back to it makes the project clean again. The window title and the macOS edited dot follow `is_dirty()`, and Open and window close ask Save / Don't Save / Cancel.

### 3.10 Plugin compatibility and safety measures

Each of these comes from a reproduced failure; details are in [troubleshooting.md](troubleshooting.md).

| Measure | Where | Why |
|---|---|---|
| Plugin runs in its own process | `vst_client`, host | a crash in the plugin (`0x40000015`, `0xC0000005`) only kills the host |
| stderr to a file, not a pipe | `vst_client` | survives the crash so it can be read afterwards |
| Private empty `TEMP` per spawn | `vst_client` (`SURF_NO_PRIVATE_TEMP` to opt out) | Kontakt's libzmq 4.3.5 aborts when `mkdir` in `%TEMP%` fails |
| cwd + DLL dir = plugin parent | `vst_client` + host | Kontakt Portable's wrapper does a bare `LoadLibrary("ktp871.dll")` |
| COM STA initialized before load, kept alive | host | plugins assume an STA COM apartment |
| Shared memory mapped before load | host | rules the IPC out when a plugin fails to load |
| 240 s `Ready` timeout | `vst_client` | Kontakt can take about 80 s to instantiate |
| `vst3-host` patches | `vendor/vst3-host` | `getSize` failure is non-fatal; no throw-away `IPlugView`; optional compatibility metadata |
| VST2 host callbacks | `vendor/vst` + `vst2.rs` | `GetSampleRate`, `GetDirectory`, `CanDo`, block size, process level |

### 3.11 Plugin discovery

- VST3 roots are scanned recursively up to depth 6. A `.vst3` file or bundle directory is one entry, named by its file stem. Stub files are skipped.
- VST2 roots are scanned recursively for this OS's VST2 type (`.dll`, `.vst` bundle, `.so`), and those entries are flagged `vst2`. On macOS the loader opens `Contents/MacOS/<CFBundleExecutable>` inside the bundle.
- Extensions are matched case-insensitively. Results are capped at 400, sorted, and deduplicated.
- `scan_folder(dir)` backs `LOAD VST`: if the folder holds one plugin it loads it directly; if it holds several, it opens the picker.

### 3.12 Configuration (environment variables)

| Variable | Effect |
|---|---|
| `SURF_VST` | load this plugin path on startup (after the first frame) |
| `SURF_VST_SCAN` | extra scan roots, separated by `;` or `,` |
| `SURF_OPEN_PLUGINS` | open the plugin picker on startup |
| `SURF_NO_STATE` | don't restore saved plugin state |
| `SURF_NO_EDITOR` | don't open the plugin editor automatically |
| `SURF_NO_PRIVATE_TEMP` | the host inherits the real `%TEMP%` |
| `SURF_NO_CHDIR` | skip the host's chdir / `SetDllDirectoryW` |
| `SURF_HELPER_LOG` | host log path (default `data_dir()/surf-vst-host.log`) |
| `SURF_AUDIO_CUSHION` | host ring target depth in frames (128–16384, default 512) |
| `SURF_STATE_DIR` | override `data_dir()` |
| `SURF_TEST_CLOSE_EDITOR_AFTER_MS` | test only (macOS): the host presses its editor's close button this long after opening; used by `editor_reopen` |

*Target:* persist user settings (last plugin, audio device, window size) in `data_dir()/settings.json`; keep the environment variables as debug overrides.

### 3.13 Testing

- **Unit tests (current, 27):** run with `cargo test --workspace --lib`. They cover the tempo map, MIDI file round trip, recorded notes → ticks, synth note/release/sustain, non-silent 24-bit stereo WAV, and six scan rules. Also covered:
  - MIDI message parsing, CoreMIDI packet walking, and a CoreMIDI loopback through a virtual source into `MidiHub` (macOS only, no hardware needed);
  - shared memory create/open (`surf-platform`), plus the shm rings across two mappings: header, MIDI order, a two-thread audio stream across several wraps, and full-ring rejection;
  - the JSON wire format of the control protocol.
- **Probes** (`examples/`): load/render/state smoke tests per format (`vst_probe`, `vst_render`, `vst2_probe`, `helper_audio`, `helper_render`) and diagnostics for host failures (`raw_load_probe`, `crash_probe`, `fdpair_probe`, `shm_probe`, `latency_probe`, `glitch_probe`).
- **Target:** `surf-core` commands get property tests (apply then undo is the identity), and CI runs the suite on Windows, macOS and Linux.

---

## Roadmap mapping

| Phase | Scope | Main sections |
|---|---|---|
| 1 ✅ | Workspace split + platform layer; macOS builds and runs built-in synth + MIDI + playback; Windows behavior unchanged | §1.6, §1.7, §2.7 |
| 2 ✅ | `surf-core` model + commands + undo; Logic-style shell; editing in Tracks and Piano Roll; Perform view in GPUI | §2.1, §2.6, §3.1 |
| 3 | Per-track instruments ✅ (Kontakt shared by MIDI channel ✅, stem export ✅); sample-accurate plugin MIDI; async export | §2.3, §2.6, §3.2, §3.8 |
| 4 | `surf-visual` stage renderer, video export | §2.8 |
| ongoing | VST3 editors on macOS/Linux, CLAP backend, settings persistence | §1.6, §2.4, §3.12 |
