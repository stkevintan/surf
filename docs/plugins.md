# Using plugin instruments

Surf plays through its built-in piano by default. It can also host VST3 and VST2 instruments. On Windows both formats are tested with real instruments, including their editors. On macOS, Kontakt 8 (VST3) is tested: it loads, saves its state, and its editor opens. VST2 audio, MIDI, state and rendering are tested there with test plugins; VST2 editors don't open on macOS yet. Every plugin runs in its own `surf-vst-host` process, so a crashing plugin never takes down the app: Surf falls back to the built-in piano and keeps playing.

## Loading an instrument

There are three ways to pick a plugin:

1. **`SCAN`** (sidebar). Lists everything installed in the standard folders. Click a row to load it. Scanning only reads the file system and never loads a plugin, so it is safe to run at any time. Press `Esc` or `CLOSE` to dismiss the list.
   - Windows. VST3: `C:\Program Files\Common Files\VST3`, `C:\Program Files (x86)\Common Files\VST3`, `%LOCALAPPDATA%\Programs\Common\VST3`. VST2 (`.dll`): `C:\Program Files\VSTPlugins`, `C:\Program Files\Steinberg\VSTPlugins`
   - macOS. VST3: `/Library/Audio/Plug-Ins/VST3`, `~/Library/Audio/Plug-Ins/VST3`. VST2 (`.vst` bundles): `/Library/Audio/Plug-Ins/VST`, `~/Library/Audio/Plug-Ins/VST`
   - Linux. VST3: `/usr/lib/vst3`, `/usr/local/lib/vst3`, `~/.vst3`. VST2 (`.so`): `/usr/lib/vst`, `/usr/local/lib/vst`, `~/.vst`
   - All folders are searched recursively. Extra folders: set `SURF_VST_SCAN`, separated by `;` or `,`.
2. **`LOAD VST`**. Choose a folder. If it contains one plugin, Surf loads it; if it contains several, a picker opens.
3. **`SURF_VST`**. Load a plugin on startup:

   ```powershell
   $env:SURF_VST = "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
   cargo run
   ```

`.vst3` paths use the VST3 backend. VST2 paths (`.dll` on Windows, `.vst` on macOS, `.so` on Linux) use the VST2 backend.

On Apple silicon, an Intel-only VST2 plugin cannot load: the plugin host is an arm64 process. Use a universal or arm64 build of the plugin.

## Plugin state

`SAVE STATE` (or `Ctrl+S`) stores the plugin's complete state, the same blob a DAW saves in a project. The next time that plugin loads, Surf restores it automatically. Set `SURF_NO_STATE=1` to skip the restore.

State files are stored in the data directory as `vst-<hash>.state`, keyed by the plugin path. The data directory is `%APPDATA%\Surf`; if that is not writable, Surf falls back to `.surf\` in the repository. The status bar shows the path it actually used. If you move the plugin, its saved state no longer matches.

## Kontakt workflow

Kontakt has no API for loading an `.nki` directly. Load the instrument in Kontakt's own editor, then let Surf save and restore the whole state:

1. Load **Kontakt 8** with `SCAN`. The official Native Access install is a single file, `C:\Program Files\Common Files\VST3\Kontakt 8.vst3`.
2. Kontakt's editor opens in the plugin host. Load your instrument, library, or multi there.
3. Click `SAVE STATE` in Surf.
4. Click `EXPORT WAV`. The plugin host creates a fresh Kontakt instance, switches it to offline mode, restores the saved state, renders the song, adds a 3-second tail, and writes a 24-bit / 48 kHz WAV.

Notes:

- The first load of Kontakt can take around 80 seconds while it scans libraries. It is not hung. Surf waits up to 240 seconds.
- For **Kontakt Portable**, load `...\Kontakt 8\x64\VST3\Kontakt 8 Portable.vst3`. Its VST2 DLL (`...\x64\VST\Kontakt 8 Portable.dll`) only initializes inside Cubase's scanner and crashes in other hosts, so it can't be used as a general VST2 alternative.
- Some direct-from-disk pianos may drop notes in offline rendering. Increase the instrument's preload buffer, or check the peak level after export. A real-time bounce option is planned.

## Current limitations

- One plugin instance at a time. Multi-track instruments are planned (see [architecture.md](architecture.md)).
- If the plugin host crashes, the current sound and any unsaved state are lost and the plugin has to be reloaded.
- Only the plugin's main stereo output is used.
- Live playback through a plugin is not latency-compensated. Exported audio is.
- WAV export through a plugin blocks the UI until it finishes.

If a plugin fails to load or crashes, see [troubleshooting.md](troubleshooting.md).
