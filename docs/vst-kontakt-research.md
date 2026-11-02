# Rust + GPUI 钢琴可视化 App：VST3 / Kontakt 与 WAV 导出调研

> 目标：做一个 SeeMusic 风格的桌面应用——实时 MIDI 输入、Live / Play 两种钢琴可视化、录制并导出 MIDI，并尽量复用 Kontakt 等 VST3 音源，最终导出带音源的 WAV。
>
> 本文记录技术选型、可行性验证、Kontakt 的特殊限制，以及本仓库已经实现的 MVP。文末列出参考链接。

---

## 0. 结论先行（TL;DR）

1. **GPUI 可以用来做这个 App**。官方 `gpui 0.2.2`（2026-10）在 Windows 上能正常编译、开窗、绘制和持续动画；本仓库已经在 Windows 11 + Rust 1.94 MSVC 上跑通。
2. **音符可视化第一版用 GPUI 的 `div` 绝对定位就够了**。屏幕内同时可见的音符通常 < 300 个，GPUI 的 GPU 批处理完全能承受。上千个音符或需要拖尾/粒子时，再换 `canvas` + `paint_quad` 或自绘 `Element`。
3. **音频用 `cpal`，MIDI 输入在 Windows 上直接用 WinMM（`midiInOpen`）**。`midir` 与 `cpal` 在同一个 lockfile 里会因为 ALSA 的 `links = "alsa"` 冲突；Windows 原生 MIDI API 反而更直接。
4. **VST3 托管用 [`vst3-host` 0.6](https://docs.rs/vst3-host/0.6.1/vst3_host/)**。它提供安全的 `Plugin` 封装、`AudioBuffers`、`set_process_mode(Offline)`、编辑器窗口 `PluginWindow` 和 state 存取，MIT 许可，适合做 MVP。
5. **Kontakt 没有公开的 “加载 NKI” API**。VST3 层面只能加载插件、发 MIDI、存/取整个 state。可行工作流是：
   - 在 App 内打开 Kontakt 原生编辑器；
   - 用户自己加载 `.nki` / 选择 Library；
   - App 调 `save_state()` 把整个 Kontakt 状态存成字节；
   - 下次启动或离线导出时，新建插件实例并 `load_state()`。
   这也是 DawDreamer 作者给出的建议（见 [issue #133](https://github.com/DBraun/DawDreamer/issues/133)）。
6. **导出 Kontakt WAV 是可行的**：新建一个离线实例，`kOffline` 模式，恢复 state，按 64 帧块送 MIDI、收音频、写 24-bit WAV，末尾加 tail，并按 `getLatencySamples()` 去掉前导延迟。Kontakt 本身有 “Offline Interpolation Quality” 设置，说明它支持离线渲染；但磁盘流（DFD）音源在极快离线渲染时可能吞音，**必须保留 1× 实时 bounce 作为兜底**。
7. **稳定性是最大风险**：Kontakt 是重量级、闭源、带 GUI/OpenGL/QML 的插件，进程内托管一旦崩溃会带走整个 App。本仓库现在默认把 VST3 放进独立 `surf-vst-host.exe`：音频/MIDI 走 Windows shared-memory ring，控制/state/render 走 JSON；helper 被 Kontakt abort 时主进程存活并回退到内置钢琴。

---

## 1. SeeMusic 类应用的功能拆解

SeeMusic 的核心体验可以拆成四层：

| 层 | 能力 | 本仓库 MVP |
|---|---|---|
| 输入 | MIDI 键盘、MIDI 文件、电脑键盘 | ✅ WinMM MIDI + 电脑键盘 A–K |
| 时间轴 | Tempo map、拍号、音符时长、循环、seek | ✅ 基本 tempo map / 播放 / 暂停 / ±5s / loop |
| 视觉 | 88 键、下落音符、键盘高亮、节拍网格 | ✅ Live 上升 / Play 下落、按键高亮、beat/bar 网格 |
| 声音 | 内置音色、SoundFont、VST3 音源 | ✅ 内置加法合成钢琴 + VST3（含 Kontakt） |
| 录制/导出 | 录 MIDI、导出 MIDI、导出 WAV | ✅ 录制 take、SMF 导出、24-bit WAV |
| 视频 | 录屏 / 离线渲染视频 | ⏳ 未实现（建议 ffmpeg / `zed-scap`） |

“Live 模式音符从下往上、Play 模式音符从上往下”在实现上只是同一个时间轴映射的两个方向：

- Play：`y_bottom = H - (note.start - t) * speed`，音符在 `t = start` 时正好落到键盘；
- Live：按住时 `height = (t - start) * speed`，从键盘向上生长；松开后整块以固定高度继续上浮。

---

## 2. GPUI 可行性

### 2.1 版本与 API

官方 crate 是 [`gpui 0.2.2`](https://docs.rs/gpui/0.2.2/gpui/)（Apache-2.0，2026-10 发布）。当前 `Application` 入口仍是：

```rust
Application::new().run(|cx: &mut App| {
    cx.open_window(WindowOptions { /* ... */ }, |window, cx| {
        cx.new(|cx| SurfApp::new(window, cx))
    })?;
    Ok::<_, anyhow::Error>(())
});
```

关键 API：

- `Render::render(&mut self, window, cx)`：每帧构建元素树。
- `window.request_animation_frame()`：在 `render` 里调用即可持续请求下一帧；GPUI 会在下一帧通知当前 view。这是做 60/120 fps 钢琴卷帘的核心。
- `cx.listener(|this, event, window, cx| ...)`：把 UI 事件绑定回 view 状态，避免手写 `Entity` 更新样板。
- `div().track_focus(&handle).on_key_down(...).on_key_up(...)`：键盘输入；配合 `window.focus(&handle)` 可以在启动时拿到焦点。
- `Canvas` / `Window::paint_quad` / `paint_path`：低层绘制入口，后续做数千音符或粒子时使用。

### 2.2 Windows 支持

`gpui 0.2.2` 的 Windows 目标依赖包含 `windows`、`windows-registry`、`blade-graphics` 等，窗口是 Win32，文字是 DirectWrite，渲染走 Blade/D3D。实测：

- `cargo build` 通过；
- 窗口能打开并持续渲染；
- 150% DPI 下需要显式设置窗口逻辑尺寸/位置，否则默认 `Bounds::centered` 在 4K 屏上可能超出屏幕。本仓库用固定的 `origin + size` 并控制高度。

### 2.3 性能建议

- 可见音符 < 300：`div` + 绝对定位 + `.rounded_sm()` + `.bg()` 足够，代码可读性最好。
- 300–2000：改用 `canvas`，在 paint 回调里 `window.paint_quad(...)`；每帧只画可见区间。
- > 2000 或要发光/拖尾：自写 `Element`，把音符实例写入 GPU instance buffer；GPUI 没有暴露 “instanced quad” 的高层 API，需要走自定义 shader/纹理，或直接用 `wgpu` 叠一层。
- 不要在 `render` 里做音频/文件 IO；所有实时状态通过 `AtomicU64` / channel 读取。

### 2.4 版本风险

GPUI 仍是 pre-1.0，API 会 breaking。建议：

- `Cargo.toml` 精确 pin `=0.2.2`；
- 把 GPUI 相关代码集中在 `src/app.rs` / `src/ui/`，方便升级；
- 备选：社区维护的 `gpui-pre`（Zed 快照）和 `gpui-component`（Longbridge），但生产项目优先官方 crate。

---

## 3. 音频与 MIDI 架构

### 3.1 线程模型

```
surf.exe（UI 进程）
  ├── GPUI / Timeline / cpal callback
  ├── WinMM MIDI 回调
  ├── RemoteInstrument：只读写 shared-memory ring
  └── VstClient：spawn / JSON 控制 surf-vst-host.exe

surf-vst-host.exe（VST helper 进程）
  ├── 主线程：VST 编辑器消息泵 + state/render 命令
  ├── 音频线程：VST3 process，按 64 帧写入 audio ring
  └── 插件崩溃只杀 helper，主进程检测到后回退到 Synth
```

- 时间轴以 **采样帧** 为唯一权威：`position_frames / sample_rate`。UI 只读，不自己累加 `Instant`，否则会和声音漂移。
- 没有音频设备时（CI/远程桌面），启动一个 “silent clock” 线程按实时速率跑同一个 `Core::render`，保证可视化仍可工作；本仓库已实现。
- MVP 的 `Core` 用 `Arc<Mutex<Core>>`，cpal 回调 `try_lock`，拿不到锁就输出静音。产品化应改为 lock-free ring buffer + 独立引擎线程；VST 编辑器打开时不能阻塞音频。
- VST3 的 `process` 必须在音频线程调用；`load_state`、`save_state`、编辑器、参数编辑必须在主线程调用。

### 3.2 MIDI 输入

`midir` 是跨平台首选，但它与 `cpal` 在 lockfile 中会冲突：

```
cpal -> alsa 0.11 (links = "alsa")
midir -> alsa 0.9  (links = "alsa")
```

Cargo 不允许同一个 native `links` 出现两个包。Windows 上直接用 WinMM：

```rust
midiInOpen(&mut handle, device_id, Some(callback as usize), Some(ctx as usize), CALLBACK_FUNCTION)
midiInStart(handle)
// callback: MIM_DATA, param1 = status | data1 << 8 | data2 << 16
```

回调里直接 `send(Command::NoteOn { .. })` 到音频线程，避免 “MIDI → UI 60fps → 音频” 带来的 16ms 抖动；同时把带 `Instant` 的事件发给 UI，用于录制和可视化。

### 3.3 MIDI 文件与录制

- 解析/写入用 `midly 0.5`；内部用 tick + tempo map 存储，秒数通过积分 tempo 得到，避免变速文件错位。
- 录制记录 `Instant` 时间戳，结束后转成 `RecordedNote { key, velocity, start, end }`，再按当前 tempo 转 tick，生成可播放的 `Song`。
- 延音踏板（CC64）在解析时把 note-off 延后到踏板抬起；录制时同理。
- 导出 type-1 SMF：track 0 放 tempo/time signature，track 1 放 note on/off；同一 tick 先 note-off 再 note-on，避免同音重叠。

---

## 4. VST3 托管

### 4.1 VST3 宿主需要做什么

VST3 不是 “加载 DLL 调一个函数” 那么简单。宿主至少要：

1. `GetPluginFactory()`，枚举 `kVstAudioEffectClass`；
2. 创建 `IComponent` 与 `IEditController`，连接 component/controller；
3. 实现 `IHostApplication`（名称、ID）；
4. 实现 `IComponentHandler`（`beginEdit` / `performEdit` / `endEdit`）；
5. `setupProcessing`（sample rate、block size、`kRealtime` / `kOffline`）；
6. 激活音频/事件 bus，`setActive(true)`；
7. 每个 block 填 `ProcessData`：`numSamples`、`inputs/outputs`、`inputEvents`、`inputParameterChanges`、`processContext`；
8. 编辑器：`IEditController::createView("editor")`，挂到 HWND，处理 `IPlugFrame::resizeView`；
9. state：`IComponent::getState/setState` + `IEditController::getState/setState`；
10. 延迟/尾音：`getLatencySamples()`、`getTailSamples()`。

`vst3-host` 把这些都封装了，MVP 不需要自己写 COM。

### 4.2 Rust 方案对比

| 方案 | 优点 | 缺点 | 适用 |
|---|---|---|---|
| [`vst3-host` 0.6](https://docs.rs/vst3-host/0.6.1/vst3_host/) | 安全封装；`Plugin`/`AudioBuffers`；`PluginWindow` 编辑器；`ProcessMode::Offline`；state/vstpreset；MIT | 本身是进程内库；需要自己加 helper/IPC（本仓库已做） | ✅ helper 内使用 |
| `vst3` / `vst3-sys` | 贴近 VST3 SDK，控制力强 | 需要自己实现 host、COM、编辑器；工作量大 | 需要深度定制 |
| [`plughost` 0.1](https://docs.rs/plughost/0.1.6/plughost/) | **独立 helper 进程**；VST3/CLAP/AU；离线 render；延迟对齐；崩溃隔离 | 无内置 GUI 嵌入；需要构建 helper exe；生态较新 | 产品化导出/隔离 |
| JUCE / DawDreamer C++ bridge | 兼容性最好；Kontakt 实战多 | C++ 构建、许可证（AGPL/商业）、FFI 复杂 | 需要最大兼容性 |
| 外部 DAW / Carla | 不用自己写宿主 | 无法嵌入 App；自动化/导出流程割裂 | 快速验证 |

本仓库选 `vst3-host`，因为它在 “能开 Kontakt 编辑器” 和 “能离线渲染” 之间平衡最好。

### 4.3 Kontakt 的特殊限制

#### 4.3.1 没有 NKI 加载 API

Kontakt 的 VST3 参数里没有 “加载某个 .nki” 这一项；它把整个 multi/instrument 状态放在 opaque state 里。DawDreamer 作者在 [issue #133](https://github.com/DBraun/DawDreamer/issues/133) 明确说：

> “There isn't a straightforward way to load an NKI file.”
>
> 方案 1：替换默认 `kontakt_def.nkm`，重新创建插件；
> 方案 2：`open_editor`，在 GUI 里选 NKI，然后 `save_state`，下次 `load_state`。

**推荐方案 2**：稳定、可重复、不需要改 Kontakt 安装目录。流程：

1. App 加载 Kontakt VST3；
2. `PluginWindow` 打开 Kontakt 原生编辑器（独立顶层窗口）；
3. 用户在 Kontakt 里加载钢琴/音色库；
4. App `save_state()` → `%APPDATA%\Surf\vst-<path-hash>.state`；
5. 下次启动/导出：新建实例 → `load_state()` → 直接就有音色。

方案 1（替换 `kontakt_def.nkm`）可以作为高级选项，但会改用户安装、依赖 Kontakt 内部默认路径，不建议产品化。

#### 4.3.2 编辑器与线程

- Kontakt 8 的 UI 是 QML/OpenGL 风格，必须在主线程创建/消息循环；`vst3-host::PluginWindow` 会创建原生窗口并跑消息循环。
- 不要把编辑器创建放在音频线程；不要在音频线程 `lock()` 等待 GUI。
- 如果要嵌入 GPUI 窗口而不是独立窗口，需要拿到 GPUI 的 HWND（`raw-window-handle`），把 `IPlugView` attach 上去，并自己处理 resize；MVP 用独立窗口更稳。

#### 4.3.3 多输出

Kontakt 可以开 16 路立体声输出。宿主必须显式激活额外 bus，否则只有 main stereo。MVP 只取 main stereo；如果要做分轨导出，需要：

- `set_bus_active` 激活所有 output bus；
- 为每个 bus 分配 buffer；
- 混音/写多轨 WAV。

#### 4.3.4 离线渲染与 DFD

Kontakt 8 手册里有 **Offline Interpolation Quality**（离线插值质量），说明它确实有离线渲染路径。实践中的坑：

- 某些 Kontakt 音色使用 Disk Streaming（DFD）。DAW 的 “faster than realtime” bounce 可能因为磁盘读不过来而出现吞音/爆音；Cubase 社区有类似报告（[Steinberg 论坛](https://forums.steinberg.net/t/glitches-with-render-in-place-track-export-with-some-kontakt-instruments/810817/5)）。
- 解决：
  1. 在 Kontakt 里给音色开 “Override Instrument's preload size”，把采样预载进内存；
  2. 降低离线速度，或直接 1× 实时 bounce；
  3. 导出后检测峰值/NaN/静音，异常时提示用户切换实时模式。
- `kOffline` 只是告诉插件 “现在不是实时”，不保证所有插件都正确实现；Kontakt 总体支持，但仍要留兜底。

#### 4.3.5 许可

- 用户必须自己拥有 Kontakt 及音色库的授权；App 不能分发 Kontakt 或音色。
- 保存的 state 可能包含音色库的绝对路径；换机器/移动目录后要重新加载。
- Kontakt Player 与完整版 Kontakt 的授权/库限制不同，产品化需要在文档里写清楚。

### 4.4 本机验证结果

在 Windows 11、Rust 1.94、`vst3-host 0.6.1` 下：

| 插件 | 结果 |
|---|---|
| Steinberg Omnivocal Beta | 加载成功；`has_editor=true`；原生编辑器窗口成功打开；state 1105 bytes；state 存取 round-trip；离线渲染 4.57s，peak 12.6% |
| Kontakt 8 | 加载成功；`has_editor=true`；state 4144 bytes；原生编辑器成功打开（`Kontakt 8 - VST3`，Library 列表可见）；离线渲染 WAV 成功，但默认状态没有音色 → peak 0.0% |
| helper 实时链路 | `helper_audio` 通过 shared memory 送 MIDI/收音频，peak 12.6% |
| helper 崩溃隔离 | 手动 kill `surf-vst-host.exe`，`surf.exe` 存活并回退到内置钢琴 |
| 内置 Synth | Demo 12.4s，peak 39% |

结论：**VST3 加载、编辑器、state、离线 process、WAV 写出链路都通了**。Kontakt 默认静音不是 bug，而是 “还没加载 NKI”；加载 state 后即可出声。

### 4.5 Kontakt 编辑器：`getSize()` 必须在 attach 之前容错

集成 Kontakt 时遇到：

```text
Could not open plugin editor: Failed to get view size
```

原因不是 Kontakt 坏了，而是两个实现细节叠加：

1. `vst3-host 0.6.1` 的 `PluginWindow::open()` 会先调 `has_editor()` 和
   `get_editor_size()`，这两个探针各自 `createView("editor")` 再释放；
   Kontakt 这类插件只容忍一个 `IPlugView`，第三个真正的 view 再 `getSize()` 就可能失败。
2. 即使只创建一个 view，Kontakt 的 QML 编辑器在 `attached(HWND)` 之前
   `IPlugView::getSize()` 也可能返回失败；而 `vst3-host` 把它当作致命错误。

本仓库对 `vendor/vst3-host` 做了两个小补丁：

- `PluginWindow::open()` 不再创建探测 view，直接用默认 1000×760 建窗；
- `open_editor()` 里 `getSize()` 失败时回退到默认尺寸，继续 `setFrame()` / `attached()`，
  让插件 attach 之后通过 `IPlugFrame` 请求 resize。

补丁后 Kontakt 8 编辑器实测可以打开并显示 Library 列表；Omnivocal 也仍然正常。
这属于 **插件兼容性 workaround**，不是 VST3 规范要求；产品化时应把这类补丁反馈给
`vst3-host` 上游，或把 Kontakt 放到独立 helper 进程里。

### 4.6 State 保存/恢复的两个实践问题

1. **路径可写性**：默认写 `%APPDATA%\Surf` 在普通桌面环境没问题，但 DSH 的
   workspace-write 沙箱会拒绝写 AppData。App 现在会先探测目录是否可写：
   - `SURF_STATE_DIR`（显式指定）
   - `%APPDATA%\Surf`
   - 工作区 `.surf/`（开发环境）
   - exe 目录 `surf-state/`
   - 临时目录
   状态栏会显示实际写入路径。实测 Kontakt 8 的 state 为 4151 bytes，
   写入 `C:\Repos\surf\.surf\vst-<hash>.state` 成功。
2. **恢复时机**：`load_state()` 必须在 `start_processing()` **之前**调用。
   如果在插件已经激活之后再恢复，Kontakt 的编辑器可能显示新音色，但音频线程
   仍停留在默认状态。本仓库的 `VstPlugin::load_with_state()` 在 load 之后、
   `start_processing()` 之前应用 state。

另外，state 文件按插件 bundle 路径 hash 命名；如果移动了 `.vst3` 目录，
hash 会变化，需要重新保存。

### 4.7 Portable 版 Kontakt 的 DLL 搜索路径坑

`Kontakt 8 Portable.vst3` 不是完整的 167MB VST3，而是一个 **1.4MB 的
`KtpVST3` 包装器**；真正插件是 `Kontakt 8.vst3plugin`，同目录还有
`ktp871.dll`。包装器用 bare `LoadLibrary("ktp871.dll")` 加载这个 DLL。

在 standalone `Kontakt 8.exe` 里，exe 所在目录就是 `x64`，DLL 搜索自然成功；
但在第三方 host 里：

- host 的工作目录通常是自己的 exe 目录；
- Windows 的 `LoadLibrary` 默认搜索 **host exe 目录 + 系统目录 + PATH**，
  不会自动搜索“被加载 DLL 的父目录”；
- 于是 `ktp871.dll` 找不到，包装器继续执行并访问空函数指针，表现为
  `Kontakt 8.vst3plugin` 内 `0xc0000005`。

修复：加载插件前把 helper 的当前目录切到 `...\Kontakt 8\x64`（或调用
`SetDllDirectoryW`/`AddDllDirectory` 加入该目录）。helper 现在在
`vst_helper.rs` 里做这件事：

```rust
if let Some(plugin_dir) = args.plugin.parent() {
    if let Some(search_dir) = plugin_dir.parent() {
        std::env::set_current_dir(search_dir)?;
    }
}
```

修复后 Kontakt 8 Portable 可以在 helper 中正常加载、打开编辑器、保存 state。
直接加载 167MB 的 `Kontakt 8.vst3plugin` 会 `LoadLibraryExW failed`；它不是
独立 VST3 bundle，必须通过包装器/正式安装的 bundle 加载。

### 4.8 Cubase 能加载、Surf 不能：它其实用 VST2

检查 `...\Cubase 15_64\Cubase Pro VST3 Cache\vst3plugins.xml`，里面没有
Kontakt；而 `Vst2xPlugin Infos Cubase.xml` 明确记录：

```text
...\Kontakt 8\x64\VST\Kontakt 8 Portable.dll
name=Kontakt 8 Portable
sdkVersion=VST 2.4
```

所以 Cubase 加载的是 **VST2** 包装器，不是 Surf 之前尝试的 VST3 包装器。
这也解释了为什么“Cubase 能开”并不能证明 VST3 wrapper 在第三方 host 里可用。

本仓库现在增加了 VST2 backend：

- `vst` crate 负责 VST2.4 host；
- `Vst2Plugin` 负责 load/init/resume、bank chunk state、VST2 editor；
- `Vst2Instrument` 负责 MIDI events + 64 帧 `processReplacing`；
- helper 按扩展名选择 backend：`.vst3` → VST3，`.dll` → VST2；
- UI 的 `LOAD VST` 现在同时接受 `.vst3` 和 `.dll`。

验证用的稳定 VST2 插件是 TruePianos：

```text
name=TruePianos outputs=2
shared-memory audio peak: 45.7%
helper rendered 12.40s, peak 70.0%
```

Kontakt portable VST2 在普通 Medium token 的 PowerShell 里**仍然**会在
`VSTPluginMain` 阶段触发 `0xC0000005`，所以这不是 DSH 的 Low token 问题。
同一台机器上：

- TruePianos VST2：generic host 加载/编辑器/音频/渲染全部正常；
- Kontakt Portable VST2：generic host 初始化阶段崩；
- Cubase 15 的 `vst2xscanner.exe -p ...`：成功，输出完整 `<vst2xplugin>`；
- 用代理 DLL 记录 scanner 的 host callback，只有：
  - `op=1 Version -> 2400`
  - `op=33 GetProductString -> 1`（空字符串）
- 写一个**最小 C++ VST2 host**，只实现这两个 callback、不经过 Rust/helper/
  shared memory：仍然在 `GetProductString` 之后 `0xC0000005`。

结论：这个 portable 包装器依赖的不只是标准 VST2 host callback，而是
Cubase scanner 进程的隐藏上下文（模块/路径/加载器/保护检查等）。generic host
无法通过补 callback 解决。已补的 `vst` host callback、隐藏窗口、STA COM、
host 字符串/进程名伪装都没有改变结果。需要稳定使用 Kontakt，应安装官方
Kontakt 8/Player 的 VST3，或继续在 Cubase 里使用它。

---

## 5. 导出使用 Kontakt 音色的 WAV

### 5.1 推荐算法（离线）

```text
load plugin (fresh instance, 48 kHz, block 64)
set_process_mode(Offline)
load_state(saved_kontakt_state)      // 关键：恢复 NKI / multi
start_processing()
latency = getLatencySamples()
total = (song.duration + tail) * sr + latency
for each 64-frame block:
    trigger note on/off at block boundaries (or sample offsets)
    plugin.process_audio(buffers)
    skip first `latency` frames
    write 24-bit stereo
write tail (3 s)
```

本仓库的 `render_with_instrument` 就是这个循环；`render_vst_to_wav` 负责新建离线实例、`kOffline`、`load_state`。24-bit PCM、48 kHz、立体声，足够交付。

### 5.2 延迟与尾音

- `getLatencySamples()` 可能来自 look-ahead/oversampling；离线时把前 `latency` 帧丢掉即可对齐。
- `getTailSamples()` 可能是 `u32::MAX`（无限）；MVP 固定 3 秒 tail，覆盖钢琴释音/踏板噪声。
- 实时监听时延迟补偿更复杂：需要把 MIDI 事件提前 `latency` 帧，或把监听信号延迟对齐；MVP 不做，文档标注。

### 5.3 失败检测与兜底

导出后检查：

- 峰值是否为 0（没加载音色 / 插件没出声）；
- 是否 NaN/Inf；
- 时长是否明显短于预期（DFD 吞音）。

UI 提供 “Offline” 与 “Realtime bounce” 两个选项：

- Offline：快，适合内存预载音色；
- Realtime：1× 速度，最稳，适合 DFD 大钢琴。

### 5.4 为什么不直接录 App 的输出？

实时录音需要 loopback / WASAPI 回环，受系统混音、其他 App 声音、采样率影响；离线渲染可重复、无实时抖动，优先。实时 bounce 只在插件不支持离线时使用。

---

## 6. 本仓库实现映射

```
src/
  app.rs             GPUI 界面：Live/Play、键盘、音符、侧栏、导出
  song.rs            tick/tempo map、MIDI 解析/写入、Demo
  recorder.rs        Live MIDI → RecordedNote
  midi_input.rs      WinMM MIDI 输入
  audio/
    mod.rs           Instrument trait、常量
    engine.rs        cpal 输出、command 队列、时间轴调度、silent clock
    synth.rs         内置加法钢琴
    render.rs        离线 WAV（24-bit）
    vst.rs           helper 内的 VST3 加载/编辑/state/离线导出
    vst2.rs          helper 内的 VST2 加载/编辑/state/离线导出
    ipc.rs           shared memory：audio ring + MIDI ring + 原子索引
    remote.rs        UI 侧 RemoteInstrument（只碰 shared memory）
    vst_client.rs    启动/控制 helper、JSON 协议、state/render 请求
  bin/
    vst_helper.rs    surf-vst-host.exe：插件 + 编辑器 + 音频线程
examples/
  vst_probe.rs       插件加载 smoke test
  vst_render.rs      VST3 离线导出 smoke test
  render_demo.rs     内置音色导出 demo
```

已实现：

- GPUI 窗口、深色 UI、88 键、下落/上升音符、节拍网格、键盘高亮；
- Live：WinMM MIDI + 电脑键盘 A–K、录音、录制 take 转 Song；
- Play：MIDI 文件加载、播放/暂停/停止/±5s/loop、进度；
- 内置 Synth、Demo、WAV 导出、MIDI 导出；
- VST3/VST2 helper 进程（`surf-vst-host.exe`）、Kontakt 编辑器、state、离线 WAV；
- shared-memory 实时音频/MIDI；手动 kill helper 后主进程存活并回退；
- 无音频设备时 silent clock；
- 10 个单元测试（tempo map、MIDI roundtrip、录制、Synth、WAV）。

未实现 / 下一步：

- helper 崩溃后自动重启/恢复 state；
- 实时监听 latency 补偿；
- Kontakt 多输出分轨；
- 实时监听延迟补偿；
- 视频导出（ffmpeg / `zed-scap`）；
- 设置持久化（上次 VST、窗口、音频设备）；
- MIDI 编辑（量化、删除、拖拽）；
- 自动加载上次 Kontakt state。

---

## 7. 风险清单

| 风险 | 影响 | 缓解 |
|---|---|---|
| Kontakt 崩溃 | helper 退出，当前 VST 丢失；主 App 存活 | `surf-vst-host.exe` 隔离 + 自动回退 |
| helper 音频 ring 下溢 | 短暂静音 | 341ms ring；监控峰值；后续加大/自适应 |
| helper 导出阻塞 UI | 导出期间界面停住 | 后续异步进度/独立 render helper |
| DFD 离线吞音 | WAV 缺音 | 预载、实时 bounce、导出后检测 |
| 插件 GUI 阻塞音频 | 爆音 | 不在音频线程锁 GUI；隔离进程 |
| VST3 版本/ABI 差异 | 某些插件加载失败 | 插件扫描 + 友好错误；保留内置音色 |
| 未加载 NKI 导致静音 | 用户以为坏了 | 导出前检测 state/峰值，提示 |
| GPUI API 变化 | 升级成本 | pin 版本、隔离 UI 层 |
| 音色库路径失效 | state 加载后无声 | 提醒用户重新加载 NKI |
| 许可 | 法务风险 | 不分发 Kontakt/音色；文档声明 |

---

## 8. 参考链接

- GPUI 官方 crate / API：<https://docs.rs/gpui/0.2.2/gpui/>
- GPUI `Window`（`request_animation_frame`、`on_next_frame`）：<https://docs.rs/gpui/0.2.2/gpui/struct.Window.html>
- `vst3-host`：<https://docs.rs/vst3-host/0.6.1/vst3_host/>
- `vst3-host::Plugin`（note/process/state/editor）：<https://docs.rs/vst3-host/0.6.1/vst3_host/plugin/struct.Plugin.html>
- `plughost`（进程隔离、离线渲染）：<https://docs.rs/plughost/0.1.6/plughost/>
- DawDreamer “load NKI” 讨论：<https://github.com/DBraun/DawDreamer/issues/133>
- Kontakt 8 手册（Offline Interpolation Quality）：<https://www.native-instruments.com/fileadmin/ni_media/downloads/manuals/kontakt/Kontakt_8_6_User_Guide_English.pdf>
- Cubase / Kontakt 离线渲染吞音讨论：<https://forums.steinberg.net/t/glitches-with-render-in-place-track-export-with-some-kontakt-instruments/810817/5>
- JUCE 论坛：hosting “plugin host” 类插件（Kontakt）：<https://forum.juce.com/t/dealing-with-plugin-host-plugins-like-kontakt/51740/2>
- VST3 Developer Portal：<https://steinbergmedia.github.io/vst3_dev_portal/pages/Technical+Documentation/Index.html>
- SeeMusic 官网：<https://seemusic.com/>
- `midly`：<https://docs.rs/midly/0.5/>
- `cpal`：<https://docs.rs/cpal/0.18/>
