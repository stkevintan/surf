# Using plugin instruments

Surf plays through its built-in piano by default. It can also host VST3 and VST2 instruments. On Windows both formats are tested with real instruments, including their editors. On macOS, Kontakt 8 (VST3) is tested: it loads, saves its state, and its editor opens. VST2 audio, MIDI, state and rendering are tested there with test plugins; VST2 editors don't open on macOS yet. Every plugin runs in its own `surf-vst-host` process, so a crashing plugin never takes down the app: Surf falls back to the built-in piano and keeps playing.

## Loading an instrument

Every track has its own instrument. Picking a plugin puts it on the **selected track** (the track header and the Inspector show which instrument each track plays). Changing a track's instrument is undoable with ⌘Z. A new track starts on the instrument of the track it was added after.

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

## Several tracks on one Kontakt

Tracks that pick the same plugin share a single instance, the way Kontakt's multi works. Each track plays on its own MIDI channel, set with the channel stepper in the Inspector. Open MIDI files get one track per channel, so a multi-channel file lines up with a Kontakt multi from the start.

1. Select the first track and pick Kontakt in the Library. Kontakt's editor opens.
2. Select each other track and pick Kontakt again. It loads instantly, since the instance is already running.
3. In Kontakt's editor, load one instrument per MIDI channel (channel 1 for the first track, 2 for the second, …), then `SAVE STATE`.

The faders still work: Surf sends each track's volume to Kontakt as CC 7 (channel volume), because Kontakt returns all its instruments already mixed. Mute and solo work as usual. When no track uses a plugin any more, its instance stops; undo starts it again with the sound it had.

Opening another MIDI file keeps the loaded instruments: each new track takes the instrument of the old track on the same channel.

## Plugin state

`SAVE STATE` (or `Ctrl+S`) stores the plugin's complete state, the same blob a DAW saves in a project. The next time that plugin loads, Surf restores it automatically. Set `SURF_NO_STATE=1` to skip the restore.

State files are stored in the data directory as `vst-<hash>.state`, keyed by the plugin path. The data directory is `%APPDATA%\Surf`; if that is not writable, Surf falls back to `.surf\` in the repository. The status bar shows the path it actually used. If you move the plugin, its saved state no longer matches.

## Kontakt workflow

Kontakt has no API for loading an `.nki` directly. Load the instrument in Kontakt's own editor, then let Surf save and restore the whole state:

1. Load **Kontakt 8** with `SCAN`. The official Native Access install is a single file, `C:\Program Files\Common Files\VST3\Kontakt 8.vst3`.
2. Kontakt's editor opens in the plugin host. Load your instrument, library, or multi there.
3. Click `SAVE STATE` in Surf.
4. Click `EXPORT WAV`. Each instrument renders its own tracks: the plugin host creates a fresh Kontakt instance, switches it to offline mode, restores the saved state and renders, while built-in piano tracks render alongside. Surf mixes them with the faders, adds a 3-second tail, and writes a 24-bit WAV at the output's sample rate.

Notes:

- The first load of Kontakt can take around 80 seconds while it scans libraries. It is not hung. Surf waits up to 240 seconds.
- For **Kontakt Portable**, load `...\Kontakt 8\x64\VST3\Kontakt 8 Portable.vst3`. Its VST2 DLL (`...\x64\VST\Kontakt 8 Portable.dll`) only initializes inside Cubase's scanner and crashes in other hosts, so it can't be used as a general VST2 alternative.
- Some direct-from-disk pianos may drop notes in offline rendering. Increase the instrument's preload buffer, or check the peak level after export. A real-time bounce option is planned.

## Current limitations

- Up to 31 instruments at once; tracks beyond that share a spare built-in piano. Tracks on a shared plugin count once.
- If a plugin host crashes, its tracks fall back to the built-in piano and any unsaved state is lost. Pick the plugin again to reload it.
- Plugin choices are not saved with the MIDI file yet; there is no project file.
- Only the plugin's main stereo output is used.
- Live playback through a plugin is not latency-compensated. Exported audio is.
- WAV export through a plugin blocks the UI until it finishes.

If a plugin fails to load or crashes, see [troubleshooting.md](troubleshooting.md).
