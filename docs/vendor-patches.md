# Vendored dependencies

`vendor/` holds two patched forks that are committed to the repository, not a build cache. `Cargo.toml` redirects them with `[patch.crates-io]`:

```toml
vst3-host = { version = "0.10", default-features = false }

[patch.crates-io]
vst3-host = { path = "vendor/vst3-host" }   # fork of 0.10.0 (MIT, HelgeSverre/rust-vst3-host)
vst       = { path = "vendor/vst" }         # fork of 0.4.0 (MIT, RustAudio/vst-rs)
```

A normal dependency can't express "0.10.0 plus three local fixes". Committing the forks also keeps `cargo update` from silently dropping them.

## `vst3-host` (63 lines over upstream 0.10.0, 3 files)

`vst3-host` was upgraded from 0.6.1 to 0.10.0 on 2026-11-02. That shrank the fork from 167 lines to 63, because upstream fixed two of the original patches, but 0.10.0 also introduced a new regression (#5 below).

| # | Problem | Status |
|---|---|---|
| 1 | Incoming CC was sent as `kLegacyMIDICCOutEvent` (a plugin-to-host event), so every CC was dropped, including the CC 64 sustain pedal | Fixed upstream in 0.9.0 (`queue_mapped_midi_controller`, `MidiMappingCache`, `IMidiMapping2`). Local patch removed. |
| 2 | `setActive(1)` was called before `setupProcessing`, with 44100 / 512 hard-coded | Fixed upstream (`set_audio_config()` after load). Local patch removed. |
| 3 | `open_editor` treats a `getSize()` failure as fatal. Kontakt's QML editor fails it before `attached()`, and some editors return `kResultOk` with an empty rect | **Local patch** in `plugin_impl.rs`: non-fatal, default 800×600 (matches `get_editor_size()` in the same file). |
| 4 | `PluginWindow::open` creates a throw-away `IPlugView` to query the size. Kontakt tolerates only one view, so the real one then fails | **Local patch** in `window.rs`: skip the pre-query, open at 800×600, and let `IPlugFrame::resizeView` report the real size after attach. |
| 5 | Regression in 0.10.0: `module_info.rs` fails the whole load when `IPluginCompatibility` metadata is unusable. Kontakt 8 declares the class but `getCompatibilityJSON` returns `kResultFalse` | **Local patch** in `module_info.rs`: log and return an empty list. The data is optional. Upstream PR #16 addresses the same issue. |
| 6 | `PluginWindow::open` (Windows) leaves the window at 800×600 after attach and waits for `resizeView`. Kontakt does not send one, so the window and editor sizes disagree | **Local patch** in `window.rs`: after `attached()`, read `attached_editor_size()` and fit the window before showing it. `resize_native_window` uses `AdjustWindowRectExForDpi` for the window's own DPI. |

#3 and #4 have no upstream issue or PR yet and are good candidates to contribute back. Upstream requires `cargo fmt`, `clippy -D warnings`, a regression test, and a Keep-a-Changelog entry.

**Before upgrading:** check whether each patch has been merged upstream; otherwise Kontakt loading breaks again. Also note the 0.9.0 breaking change: a controller the plugin has not mapped is now dropped silently (still returns `Ok(())`).

## `vst` (VST2)

Fork of `vst-rs` 0.4.0, which is no longer maintained. It adds the host callbacks real plugins query during load: `GetSampleRate`, `GetDirectory`, `CanDo`, block size, process level. Verified with TruePianos (load, editor, real-time audio, offline render).
