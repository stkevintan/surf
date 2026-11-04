# Troubleshooting

## Build

### `ring` / `cl.exe`: "failed to run custom build command" with empty stderr

`cl.exe` writes fatal errors to stdout, so Cargo's stderr shows nothing. The real error is usually:

```text
fatal error C1034: stdint.h: no include path set
```

The shell does not have the MSVC / Windows SDK environment loaded, or `INCLUDE` / `LIB` point at more than one Visual Studio version. Fix it with either of these:

```powershell
# Use the wrapper script, which loads vcvars64.bat first
powershell -ExecutionPolicy Bypass -File tools/build.ps1 build

# Or build from an "x64 Native Tools Command Prompt for VS"
cargo build
```

If an earlier failure left partial output behind, run `cargo clean -p ring`.

### "spurious network error … Timeout was reached"

Large crates (`windows`, GPUI's dependency tree) can exceed Cargo's 30-second per-crate download timeout on a slow link. `.cargo/config.toml` raises `net.retry` to 10, which is usually enough. Run the build again.

## Plugins

### Where the logs are

| File | Contents |
|---|---|
| `<data dir>\surf-vst-host.log` | plugin host stderr, appended per run (override with `SURF_HELPER_LOG`) |
| `<data dir>\surf-vst-host-panic.log` | Rust panics inside the plugin host |

`<data dir>` is `%APPDATA%\Surf`, falling back to `.surf\` in the repository. When a load fails, Surf prints that run's part of the log to its own stderr and shows the log path in the status bar.

The host's stderr goes to a file, not a pipe, so the log survives a crash.

### Narrowing down a crash

```powershell
Remove-Item Env:SURF_VST            # 1. start without a plugin: is the app itself stable?
$env:SURF_NO_STATE  = "1"           # 2. load the plugin without restoring saved state
$env:SURF_NO_EDITOR = "1"           # 3. load the plugin without opening its editor
Get-Process surf,surf-vst-host,Kontakt* -ErrorAction SilentlyContinue | Stop-Process -Force   # 4. clear leftovers
```

If it only crashes after loading a particular `.nki` or state, the problem is in the plugin, library, or state file.

Check the Windows event log for the faulting module:

```powershell
Get-WinEvent -FilterHashtable @{LogName='Application'; Id=1000; StartTime=(Get-Date).AddMinutes(-10)} |
  Where-Object Message -match 'surf-vst-host|Kontakt'
```

If the faulting module is the plugin itself (for example `Kontakt 8.vst3plugin`, `0xc0000005`), run the plugin's standalone app. If the standalone also crashes, reinstall the plugin or repair the VC++ runtime.

### Kontakt aborts with `0x40000015` and `epoll.cpp: Bad file descriptor`

Kontakt statically links libzmq 4.3.5, which ignores a failed `mkdir` in `%TEMP%` and then aborts. Surf avoids this by giving each plugin host a private, empty `%TEMP%` (`<data dir>\helper-tmp`). Set `SURF_NO_PRIVATE_TEMP=1` to reproduce the original behavior.

The most common cause of an unwritable `%TEMP%` is a **Low integrity label on the repository folder**. Every binary built inside it then runs at low integrity, and that one cause produced three separate symptoms:

- official Kontakt aborts in libzmq (above);
- Kontakt Portable crashes in `createInstance` with `0xC0000005`;
- the `writes_non_silent_wav` unit test fails with "access denied" (os error 5).

Check and fix it, then rebuild:

```powershell
icacls <repo> | Select-String "Mandatory Label"
icacls <repo> /setintegritylevel "(OI)(CI)medium"
```

### Kontakt Portable: "VST3 helper process exited"

The portable `Kontakt 8 Portable.vst3` is a small wrapper that calls `LoadLibrary("ktp871.dll")` from its parent folder. The plugin host starts in the plugin's parent directory and adds it to the DLL search path, which fixes this. Set `SURF_NO_CHDIR=1` to disable that if you need to rule it out.

The symlink `C:\Program Files\Common Files\VST3\Kontakt 8 Portable` breaks if the portable package folder is renamed. Load the real path under `...\Kontakt 8\x64\VST3\` instead, or rerun the package's `Install.exe`.

### Kontakt seems to hang on load

On a cold cache, Kontakt 8 can take around 80 seconds to return from `createInstance` while it scans libraries at full CPU. Surf waits up to 240 seconds before giving up. Give manual tests at least that long too.

### Audio crackles with a plugin

The plugin host keeps about 10.7 ms (512 frames) of audio queued. On a busy machine, raise it:

```powershell
$env:SURF_AUDIO_CUSHION = "1024"   # frames, 128–16384
```

## Diagnostic examples

Each probe in `examples/` isolates one step of the plugin pipeline:

| Example | What it checks |
|---|---|
| `vst_probe <plugin.vst3>` | load and report editor / state |
| `vst_render <plugin.vst3> <out.wav>` | offline render in-process |
| `helper_audio <plugin>` | full plugin host + shared-memory real-time path |
| `helper_render <plugin> <out.wav>` | offline render inside the plugin host |
| `vst2_session <plugin>` | load through the plugin host, play a note, save and restore state, request the editor |
| `editor_hold <plugin> [seconds]` | open the plugin's editor through the plugin host and keep it open |
| `vst_state_render <plugin> <state> <out.wav>` | load with a saved state file and render, to check the state changes the sound |
| `raw_load_probe <plugin>` | only `LoadLibrary` → `InitDll` → `GetPluginFactory` |
| `crash_probe <plugin>` | prints the faulting module via a vectored exception handler |
| `fdpair_probe` | whether the process can create the socket pair libzmq needs |
| `shm_probe <plugin>` | maps shared memory first, then loads the plugin |
| `vst_scan_list` | prints what `SCAN` would list |

```powershell
cargo run --example helper_audio -- "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
```

A peak of 0% from Kontakt is expected when no instrument is loaded in its state.
