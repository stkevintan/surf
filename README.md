# Surf — Rust + GPUI 钢琴可视化 / MIDI Studio

一个 SeeMusic 风格的桌面应用：接 MIDI 键盘，实时把音符画成钢琴卷帘；播放 MIDI 文件时音符从上往下落到键盘；可以录制现场演奏、导出 MIDI，并把内置音色或 VST2/VST3 音源（包括 Kontakt）离线渲染成 WAV。

> 技术栈：Rust 2024 · GPUI 0.2.2 · cpal 0.18 · midly 0.5 · hound 3.5 · vst3-host 0.6
>
> 平台：Windows 10/11 x64（MIDI 输入使用 WinMM，VST3 使用 Windows 原生窗口）

![Surf screenshot](docs/app.png)

---

## 功能

- **Live 模式**：MIDI 键盘 / 电脑键盘实时发声，音符块从键盘向上生长；支持延音踏板。
- **Play 模式**：打开 `.mid` / `.midi`，音符从上方落到键盘，键盘按键高亮；播放/暂停/停止、±5 秒、循环。
- **录音**：Live 模式下按 `RECORD` 开始，停止后自动把 take 转成可播放的 Song，可直接播放或导出 MIDI。
- **可视化**：88 键、按音高着色、力度控制透明度、节拍/小节网格、当前音符高亮。
- **声音**：内置加法合成钢琴（无需任何音源即可用）；可加载 VST2/VST3 音源（Kontakt、Omnivocal、Pianoteq、TruePianos 等）。
- **插件扫描**：侧栏 `SCAN` 递归扫标准 VST3/VST2 目录，列出装了什么，点一行就加载；不 `LoadLibrary`，不会把 UI 拖崩。
- **VST 进程隔离**：插件、Kontakt 编辑器和 native 线程运行在独立 `surf-vst-host.exe`；helper 被 Kontakt abort 时主 App 存活并回退到内置钢琴。
- **导出**：
  - MIDI：标准 type-1 SMF；
  - WAV：24-bit / 48 kHz 立体声，内置音色或当前 VST3 乐器离线渲染。
- **Kontakt 工作流**：App 内打开 Kontakt 原生编辑器加载 `.nki`，保存 state；导出时新建离线实例恢复 state 渲染。编辑器已实测可打开（见 `docs/kontakt-editor.png`）。
- **无声卡兜底**：没有可用音频设备时自动跑 silent clock，界面/时间轴仍可用（方便远程桌面/CI）。

---

## 构建与运行

前置：

- Rust stable（本项目用 1.94 验证）；
- Visual Studio Build Tools 2026（MSVC + Windows SDK）；
- 可选：一块支持 ASIO/WASAPI 的声卡；
- 可选：已授权的 Kontakt + 音色库。

```powershell
# 推荐（本机环境；自动加载 vcvars + 使用工作区依赖缓存）
# 先 build 同时生成 surf.exe 和 surf-vst-host.exe，再 run
powershell -ExecutionPolicy Bypass -File tools/build.ps1 build
powershell -ExecutionPolicy Bypass -File tools/build.ps1 run

# 或者在 “x64 Native Tools Command Prompt for VS 2026” 里
cargo build
cargo run
```

首次构建会下载 GPUI 的完整依赖树（约 500+ 个 crate），需要几分钟。本仓库已经构建好一个调试版，可以直接运行：

```powershell
.\target\debug\surf.exe
```

### 网络说明

cargo 直连 crates.io 正常。只是下载体积大的 crate（`windows`、GPUI 的依赖树）偶尔会撞上
cargo 默认的「单个 crate 30 秒」超时，表现为：

```text
warning: spurious network error (3 tries remaining): [28] Timeout was reached
```

`.cargo/config.toml` 里把 `net.retry` 提到 10 就够了，重试一般就过。

> 历史记录：本仓库早期把这个超时误判成 Schannel 证书链问题（`SEC_E_NO_CREDENTIALS`），
> 为此带了一个本地 HTTP 镜像（`tools/crates_proxy.py` + `.cargo/local-proxy.toml`）。
> 2026-11-02 用空 `CARGO_HOME` 实测确认是慢、不是坏，镜像已删除。

### Kontakt 崩溃排查开关

如果 `surf.exe` 在 Kontakt 加载/编辑器/状态恢复时直接退出（Windows 事件日志异常码 `0x40000015` / `1073741845`，并伴随 `zeromq epoll.cpp: Bad file descriptor`），这是 Kontakt 原生代码触发的 `abort()`，Rust 侧无法 catch。

**先看下面的 `%TEMP%` 一节**——`epoll.cpp: Bad file descriptor` 这个具体症状已经定位并修好了，原因是 libzmq 4.3.5 在 `%TEMP%` 里 mkdir 失败后用了未初始化的 fd。下面的隔离排查用于其它崩法：

```powershell
# 1) 不自动加载 VST（确认 App 本身稳定）
Remove-Item Env:SURF_VST

# 2) 加载 Kontakt 但不恢复 state
$env:SURF_NO_STATE="1"

# 3) 加载 Kontakt 但不打开 QML 编辑器
$env:SURF_NO_EDITOR="1"

# 4) 启动前清掉残留进程
Get-Process surf,Kontakt*,NI* -ErrorAction SilentlyContinue | Stop-Process -Force
```

如果只有加载某个 `.nki` 或某个 state 后崩溃，那是 Kontakt/音色库/state 的问题。**现在 VST3 已经运行在独立 `surf-vst-host.exe` 中**：helper 被 Kontakt abort 时，Surf 主进程会检测到并自动回退到内置钢琴，不会再被 `0x40000015` 带走。

如果看到：

```text
VST3 helper load failed: VST3 helper process exited
```

先确认是不是 Kontakt portable 的 wrapper 问题。这个包的 `Kontakt 8 Portable.vst3` 只有 1.4MB，实际是 `KtpVST3` 包装器；真正插件是 167MB 的 `Kontakt 8.vst3plugin`，并且依赖同目录的 `ktp871.dll`。第三方 host 默认不会把 `x64` 目录放进 DLL 搜索路径，wrapper 调用 `LoadLibrary("ktp871.dll")` 就会失败并触发 `0xc0000005`。

**helper 已修复**：加载前会把工作目录切到插件父目录（`...\Kontakt 8\x64`），和 standalone 的行为一致。现在 Kontakt helper 可以正常加载并打开编辑器。

**根因：仓库目录被打了 Low integrity 标签（2026-11-02 定位）**

`C:\Repos\surf` 上有 `Mandatory Label\Low Mandatory Level`，并且 `(OI)(CI)` 继承给 `target\` 下所有产物。于是这个仓库编译出来的每个 exe 都以**低完整性级别**运行，而低 IL 进程**不能写普通的 `%TEMP%`**（medium IL）。这一个原因同时造成了三个现象：

- 官方 Kontakt 8：libzmq 在 `%TEMP%` 里 mkdir 失败 → `epoll.cpp:73` abort（下面详述）；
- Kontakt Portable：wrapper 在 `createInstance` 里 `0xC0000005`（`-1073741819`）；
- 单测 `audio::render::tests::writes_non_silent_wav` 写 WAV 到 `%TEMP%` 失败（os error 5）。

修掉标签后三者全部正常：

```powershell
# 查看
icacls C:\Repos\surf | Select-String "Mandatory Label"
# 清掉（见 .surf/fix_integrity.ps1），然后重新 build
icacls C:\Repos\surf /setintegritylevel "(OI)(CI)medium"
```

A/B 实测（同一个 binary、同一个插件，只有 IL 不同）：Portable 在 low IL 下 2/2 崩，medium IL 下 2/2 加载成功（`name: Kontakt 8`、`has_editor: true`）。

**为什么低 IL 会让 Kontakt abort（官方版的具体机制）**

Kontakt 静态链接了 libzmq 4.3.5。它的 `create_ipc_wildcard_address` 先 `_wtmpnam_s` 再 `_wmkdir`，**但不检查 `_wmkdir` 的返回值**。mkdir 失败时 `signaler_t` 的 `_r`/`_w` 描述符保持未初始化，这个垃圾值被传进 `epoll_ctl(EPOLL_CTL_ADD)`，EBADF 触发 `epoll.cpp:73` 的 `errno_assert`，直接 `abort()`：

```text
Bad file descriptor (D:\NIBuild\3rdparty\zeromq-v4.3.5-R2\src\epoll.cpp:73)
VEH: code=0x40000015 (STATUS_FATAL_APP_EXIT)   exit=21
```

上游 libzmq 在 2024-08 才修掉这个未检查的调用（issue #4730 / #4084），晚于 4.3.5 发布，所以 host 侧唯一的办法是**保证那个 mkdir 能成功**。低 IL 进程在 medium IL 的 `%TEMP%` 里 `_wmkdir` 必然 ACCESS_DENIED（errno 13 / `GetLastError=5`），换成一个仓库内（低 IL 可写）的空目录就成功。A/B 实测：

```text
默认 %TEMP%：exit=21，zmq abort      ×3
私有 %TEMP%：exit=0，name: Kontakt 8  ×3
```

所以 `VstClient` 仍然给 helper 单独开一个空的 `%TEMP%`（`<data dir>\helper-tmp`，每次 spawn 前清空）——这在标签已经修好之后属于冗余的安全网，但对任何 `%TEMP%` 不可写的环境都有效。注意 `tmpnam_s` 在**首次调用时缓存** temp 路径，所以只能在进程启动前通过环境变量设置，helper 内部改不了。应急开关：`SURF_NO_PRIVATE_TEMP=1`。

**注意**：`C:\Program Files\Common Files\VST3\Kontakt 8 Portable` 这个符号链接已经失效（指向被改名的 `Native Instruments - Noire v1.1.0等21个文件`）。要加载 portable 版请直接用 `...\Kontakt 8\x64\VST3\Kontakt 8 Portable.vst3` 的真实路径，或重新跑 portable 包里的 `Install.exe`。

**已更正的旧结论**：之前记录的 “stderr 是管道导致 abort” 不成立——stderr 重定向到普通文件时崩溃完全一样。helper 的 stderr 仍然写文件，因为这样崩溃后日志还在，方便事后排查：

```text
<data dir>\surf-vst-host.log      # 默认
$env:SURF_HELPER_LOG="C:\path\to\helper.log"   # 覆盖
```

`<data dir>` 是 `%APPDATA%\Surf`，在沙箱里会自动退到工作区 `.surf\`（见 `src/paths.rs`）。加载失败时状态栏也会显示这个日志路径。Rust 侧的 panic 另写到 `surf-vst-host-panic.log`，同一目录。

**Kontakt 很慢，不要误判成卡死**：官方 Kontakt 8.14.1 在这台机器上**要 ~80 秒**才从 `createInstance` 返回（期间满核跑，没有网络活动）。`VstClient` 等 `Ready` 的超时原来是 30 秒，会把本来能成功的加载提前杀掉，现在放宽到 240 秒。排查时也要给足时间：40 秒的 `timeout` 看起来像 hang，其实只是还没跑完。

验证方式：

```powershell
cargo run --example helper_audio -- "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
# exit 0、音频 peak 0%（没加载音色时属正常）；首次加载要等 ~80s
```

分段定位加载失败在哪一步可以用 `examples/raw_load_probe.rs`（只做 `LoadLibrary` → `InitDll` → `GetPluginFactory`，不创建实例）。实测这三步对 Kontakt 全部成功，崩溃发生在之后的 `createInstance`，所以 DLL 搜索路径不是这个问题的原因。`examples/crash_probe.rs` 用 VEH 打印出错模块，`examples/fdpair_probe.rs` 检查进程能否建出 libzmq signaler 需要的 socket 对。

其它调试开关：`SURF_NO_PRIVATE_TEMP=1` 让 helper 继承外部 `%TEMP%`（复现上面的崩溃）；`SURF_NO_CHDIR=1` 跳过 helper 的切目录/`SetDllDirectoryW`；`SURF_OPEN_PLUGINS=1` 启动时直接打开插件清单；`SURF_VST_SCAN` 追加扫描目录。排查 helper 启动顺序（共享内存先映射、再加载插件）可以用 `examples/shm_probe.rs`。

如果仍失败，用 Windows 事件查看器确认：

```powershell
Get-WinEvent -FilterHashtable @{LogName='Application'; Id=1000; StartTime=(Get-Date).AddMinutes(-10)} |
  Where-Object Message -match 'surf-vst-host|Kontakt'
```

如果错误模块是 `Kontakt 8.vst3plugin`、异常码 `0xc0000005`，再直接运行 Kontakt 独立程序：

```powershell
& "C:\...\Kontakt 8\x64\Kontakt 8.exe"
```

- Kontakt 独立程序也崩溃：这是 Kontakt/portable 打包安装本身的问题，不是 helper/IPC。重装官方 Kontakt、修复 VC++ Runtime、重启或联系 NI。
- Kontakt 独立程序正常，只有 helper 崩溃：把事件日志中的模块/偏移发我，再针对 helper 环境排查。

用 Omnivocal 可以验证 helper 链路：

```powershell
cargo run --example helper_audio -- "C:\Program Files\Common Files\VST3\OmnivocalBeta.vst3"
cargo run --example helper_render -- "C:\Program Files\Common Files\VST3\OmnivocalBeta.vst3" .cache\omnivocal.wav
```

### `ring` / `cl.exe` 构建失败

`ring` 的 build script 会调用 MSVC 的 `cl.exe` 编译 C 代码。如果遇到：

```text
error: failed to run custom build command for `ring v0.17.14`
--- stderr
(empty)
```

注意 **`cl.exe` 的 fatal error 默认写到 stdout**，所以 Cargo 的 stderr 可能是空的。常见实际错误是：

```text
fatal error C1034: stdint.h: 不包括路径集
```

也就是当前 shell 没有加载正确的 MSVC/Windows SDK 环境（或同时装了多个 VS 版本，`INCLUDE`/`LIB` 指向旧版本）。解决：

```powershell
# 方式 1：用封装脚本（推荐）
powershell -ExecutionPolicy Bypass -File tools/build.ps1 build

# 方式 2：在正确的 Developer Prompt 里
cmd /c "call ""C:\Program Files (x86)\Microsoft Visual Studio\18\BuildTools\VC\Auxiliary\Build\vcvars64.bat"" && cargo run"

# 如果之前失败留下半成品：
cargo clean -p ring
```

不要在普通 PowerShell 里直接 `cargo run` 然后期待它自动找到 MSVC 头文件；`cc-rs` 虽然能找到 `cl.exe` 路径，但 `INCLUDE`/`LIB`/`PATH` 混用多个 VS 版本时仍会失败。

---

## 使用

### 键盘/传输

| 操作 | 说明 |
|---|---|
| `LIVE` / `PLAY` | 切换模式 |
| `OPEN MIDI` | 打开 MIDI 文件并开始播放 |
| `DEMO` | 加载内置 4 小节 Am–F–C–G 示例 |
| `▶ / II` | 播放 / 暂停 |
| `-5 / +5` | 相对 seek |
| `RECORD` | Live 录音开关 |
| `@` | 循环 |
| `Space` | 播放/暂停 |
| `Ctrl+S` | 保存当前 VST 状态（等同侧栏 `SAVE STATE`） |
| `A W S E D F T G Y H U J K O L P ;` | 电脑键盘弹奏 C4–E5 |

> 电脑键盘需要窗口/键盘区域有焦点；点击一下 App 即可。

### 加载音源：`SCAN` / `LOAD VST` / `SURF_VST`

三种方式，任选一种：

1. **`SCAN`**：扫描标准 VST3 目录（`C:\Program Files\Common Files\VST3`、`Program Files (x86)\Common Files\VST3`、`%LOCALAPPDATA%\Programs\Common\VST3`）和 VST2 目录（`Program Files\VSTPlugins`、`Program Files\Steinberg\VSTPlugins`），把结果列成清单，点一行就加载。扫描只走文件系统、不 `LoadLibrary` 任何插件，所以在 UI 线程跑也安全（Kontakt 只会在 helper 里被加载）。`Esc` 或 `CLOSE` 关掉清单。
   想扫别的目录（例如解压在别处的 Kontakt），用 `SURF_VST_SCAN` 追加，`;` 或 `,` 分隔：

   ```powershell
   $env:SURF_VST_SCAN="D:\Plugins\Kontakt 8\x64"
   ```

2. **`LOAD VST`**：选一个文件夹。里面只有 1 个插件就直接加载，有多个就弹出清单让你选。
3. **`SURF_VST`**：启动时直接加载指定路径（固定用同一个 Kontakt multi 时最省事）：

   ```powershell
   $env:SURF_VST="C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
   cargo run
   ```

### Kontakt 导出 WAV 的完整流程

1. 点侧栏 `SCAN`，在清单里点 `Kontakt 8`；或者点 `LOAD VST` 选一个包含它的文件夹。

   官方 Native Access 装出来的 Kontakt 是**单文件** `C:\Program Files\Common Files\VST3\Kontakt 8.vst3`（约 180 MB，不是 macOS 那种 bundle 目录），所以 `SCAN` 会直接列出它；`LOAD VST` 因为只能选文件夹，要么选它所在目录（会列出目录里所有插件），要么直接用 `SURF_VST`。

   对 portable 包，正确路径是：

   ```text
   ...\Kontakt 8\x64\VST3\Kontakt 8 Portable.vst3
   ```

   `...\Kontakt 8\x64\VST\Kontakt 8 Portable.dll` 是 **VST2**。Surf 支持 VST2，但 Kontakt Portable 的这个 VST2 DLL 只在 Cubase 的 scanner 环境里能初始化；generic host（包括最小 C++ VST2 host）会在 `VSTPluginMain` 阶段 `0xC0000005`。所以这个 DLL 不能作为通用的 Kontakt VST2 替代品。

   VST2 backend 已用 TruePianos 验证（加载/编辑器/实时音频/离线渲染）。VST2 和 VST3 是两套独立 backend，不要混用 `.vst3` 路径和 `.dll` 路径。
2. `surf-vst-host.exe` 会打开 Kontakt 的原生编辑器窗口（独立 helper 进程）。
3. 在 Kontakt 里加载你的 `.nki` / Library / Multi。
4. 回到 Surf，点 `SAVE STATE`（或 `Ctrl+S`）保存整个 Kontakt 状态。路径优先 `%APPDATA%\Surf\vst-<hash>.state`；如果该目录不可写（例如 DSH 沙箱），会自动落到工作区 `.surf\vst-<hash>.state`，状态栏会显示实际路径。
5. 点 `EXPORT WAV`。helper 会：
   - 新建一个独立的 Kontakt 实例；
   - `set_process_mode(Offline)`；
   - `load_state()` 恢复你刚保存的音色；
   - 按 64 帧块送 MIDI、收音频；
   - 加 3 秒 tail，写 24-bit WAV。
6. 如果某些 DFD 大钢琴在离线模式下吞音，请改用实时 bounce（当前 MVP 的兜底是：先把 Kontakt 音色预载/调大 preload，或导出后检查峰值；实时 bounce 界面选项在 roadmap 中）。

> **注意**：Kontakt 没有公开的 “加载 NKI” API。不要尝试在代码里直接读 `.nki` 塞给插件——不可行。正确做法就是 “GUI 加载 + state 保存/恢复”，或者高级用户替换 `kontakt_def.nkm` 默认 multi（不推荐）。

---

## 架构

```
src/
  app.rs            GPUI 视图：顶栏/传输/侧栏/钢琴卷帘/键盘
  song.rs           tick + tempo map、MIDI 解析/写入、Demo
  recorder.rs       Live MIDI -> RecordedNote
  midi_input.rs     WinMM midiInOpen 回调 -> Command + UI 事件
  audio/
    mod.rs          Instrument trait
    engine.rs       cpal 输出、命令队列、frame 时间轴、silent clock
    synth.rs        内置加法钢琴
    render.rs       离线 24-bit WAV、延迟补偿、tail
    vst.rs          helper 内的 VST3 加载/编辑器/state（也用于离线渲染）
    vst2.rs         helper 内的 VST2 加载/编辑器/state/离线渲染
    ipc.rs          Windows shared memory：音频 ring + MIDI ring
    remote.rs       UI 侧 RemoteInstrument（只读写 shared memory）
    vst_client.rs   启动/控制 helper、JSON 协议、state/render 请求、helper 日志
    vst_scan.rs     插件目录扫描（只走文件系统，不加载插件）
  paths.rs          可写数据目录（%APPDATA%\Surf → .surf → exe 旁 → temp）
  bin/
    vst_helper.rs   surf-vst-host.exe：独立进程里的插件/编辑器/音频线程
examples/
  vst_probe.rs      插件加载 smoke test
  vst_render.rs     VST3 离线导出 smoke test
  vst_scan_list.rs  打印 SCAN 会列出的插件
  shm_probe.rs      共享内存先映射再加载插件（排查 helper 崩溃）
  render_demo.rs    内置音色导出 demo
docs/
  vst-kontakt-research.md   详细调研（GPUI / VST3 / Kontakt / WAV）
vendor/
  vst3-host/                小补丁：跳过 editor view 探测，getSize 失败时用默认尺寸
```

关键设计：

- **音频线程是唯一时钟**：位置用采样帧，UI 只读原子变量；没有声卡时 silent clock 跑同一套逻辑。
- **MIDI 回调直达音频线程**：减少 UI 帧率带来的延迟；同时发一份带 `Instant` 的事件给 UI 做录制/可视化。
- **VST3 独立 helper 进程**：`surf.exe` 启动 `surf-vst-host.exe`；插件、Kontakt 编辑器、native 线程都在 helper 里。插件崩溃时主进程只失去 VST，自动回退到内置钢琴。
- **实时音频**：helper 按 64 帧渲染，写入 shared-memory ring；UI 的 cpal 回调只做 ring 读取。MIDI 走另一个 shared-memory ring，控制/state/render 走 stdin/stdout JSON。
- **导出也在 helper 里**：helper `kOffline` + 恢复 state + latency 前导裁剪 + tail，UI 进程不会再次加载 VST3。

---

## 测试

```powershell
$env:CARGO_HOME="$PWD\.cargo-home"
cargo test --lib
```

当前 15 个测试：

- tempo map 正反变换；
- MIDI 写读 roundtrip；
- 录制事件 → tick；
- Synth 发声/释放/踏板；
- WAV 非静音、24-bit、双声道；
- 插件扫描：文件 stem 当显示名、bundle 目录同样处理、VST2 标记、`.vst3` 占位文件跳过、扩展名大小写不敏感。

额外 smoke test：

```powershell
# 内置音色导出
cargo run --example render_demo -- .cache\surf-demo.wav

# VST3 加载
cargo run --example vst_probe -- "C:\Program Files\Common Files\VST3\OmnivocalBeta.vst3"

# SCAN 会列出什么
cargo run --example vst_scan_list

# VST3 离线渲染（Kontakt 默认无音色时 peak=0，属正常）
cargo run --example vst_render -- "C:\...\Kontakt 8.vst3" .cache\kontakt.wav
```

本机验证结果（Windows 11 / Rust 1.94 / vst3-host 0.6.1）：

- GPUI 窗口正常渲染；
- Omnivocal Beta：helper 加载 / 编辑器 / state 1105 B / 离线渲染 4.57s，peak 12.6%；
- 官方 Kontakt 8.14.1（`C:\Program Files\Common Files\VST3\Kontakt 8.vst3`）：`SCAN` 能列出；helper 加载成功（`plugin loaded: Kontakt 8`），peak 0.0%（未加载音色）；修复点是 helper 的 stderr 改写成文件而不是管道；
- `SCAN` 在本机列出的 6 个插件：Groove Agent SE、HALion Sonic、Kontakt 8、OmnivocalBeta、TruePianos（VST2 x2）；
- Kontakt 8 Portable VST3：helper 加载成功 / 编辑器窗口成功打开 / state 4143 B；关键修复是加载前把 cwd 切到 `Kontakt 8\x64`，让 `KtpVST3` 包装器找到 `ktp871.dll`；默认状态无音色 → peak 0.0%（加载 NKI state 后即可出声）；
- TruePianos VST2：helper 加载 / 编辑器 / 实时音频 peak 45.7% / 离线渲染 12.4s peak 70.0%，用于验证 VST2 backend；
- Kontakt VST2：Cubase 15 的 VST2 缓存确认它加载的是 `...\VST\Kontakt 8 Portable.dll`（VST2 2.4）；Surf 现在走同一 backend；
- `helper_audio`：shared-memory 实时链路 peak 12.6%；
- `helper_render`：helper 进程内离线渲染 12.4s，peak 12.6%；
- **崩溃隔离**：手动 kill `surf-vst-host.exe`，`surf.exe` 仍存活，状态栏显示“VST3 helper crashed; switched to built-in piano”；
- 内置 Synth demo：12.4s，peak 39%。

---

## 已知限制 / Roadmap

- **VST helper 已隔离**：Kontakt 崩溃只会杀掉 `surf-vst-host.exe`；Surf 会回退到内置钢琴。但 helper 崩溃后当前音色/state 会丢失，需要重新加载。
- **VST2 backend**：基于已弃用的 `vst` crate（vendored 并补了 `GetSampleRate`/`GetDirectory`/`CanDo` 等 host callback）。TruePianos VST2 已验证可用；但 Kontakt Portable VST2 在普通 Medium shell、甚至最小 C++ VST2 host 里仍会在 `VSTPluginMain` 阶段 `0xC0000005`，而 Cubase scanner 能成功。说明这个 portable 包装器依赖 Cubase scanner 的隐藏上下文，不是通用 VST2 插件。稳定使用 Kontakt 建议官方 VST3。
- **helper 的音频缓冲**：shared-memory ring 固定约 341ms；CPU 抖动极大时可能 underrun，界面会表现为短暂静音。
- **导出是阻塞的**：VST3 WAV 导出在 helper 中同步执行，导出期间 UI 会停住；后续可改成异步进度。
- **helper 路径**：`surf-vst-host.exe` 必须和 `surf.exe` 同目录；`cargo run` 前需 `cargo build` 两个 bin（`tools/build.ps1 build` 会一起构建）。
- **只取 Kontakt main stereo**：多输出分轨未实现。
- **VST3 状态保存路径**：默认 `%APPDATA%\Surf`，沙箱/权限不足时回退到工作区 `.surf`；如果换了插件 bundle 路径，状态文件 hash 会变，需要重新保存。
- **`vendor/vst3-host` 是打过补丁的 fork**：详见下面「vendor 补丁」一节。升级 `vst3-host` 前必须先确认这些补丁是否已被上游合并，否则会退化。插件通过 `IPlugFrame` 请求的 resize 还没有完全同步到宿主窗口。
- **实时监听未做延迟补偿**：VST 报告 latency 时，监听可能有一点点延迟；离线导出已做前导裁剪。
- **无视频导出**：后续用 ffmpeg / `zed-scap` 把钢琴卷帘录成 MP4。
- **设置不持久化**：上次 VST、窗口大小、音频设备尚未保存。
- **MIDI 编辑**：仅播放/录制/导出，没有量化、拖拽、删除。
- **GPUI 仍是 pre-1.0**：已 pin `0.2.2`，升级时集中在 `app.rs`。

详细调研、Kontakt 内部机制和参考链接见 [docs/vst-kontakt-research.md](docs/vst-kontakt-research.md)。

---

## vendor 补丁

`vendor/` 不是构建缓存，而是**提交进仓库的 fork**。`Cargo.toml` 用 `[patch.crates-io]`
把依赖指向它们：

```toml
vst3-host = { version = "0.10", default-features = false }

[patch.crates-io]
vst3-host = { path = "vendor/vst3-host" }   # fork of 0.10.0 (MIT, HelgeSverre/rust-vst3-host)
vst = { path = "vendor/vst" }               # fork of 0.4.0
```

这样做的原因是：这些是对别人已发布 crate 的修补，没法用普通依赖表达「0.10.0 加上两个本地
修复」。放进仓库可以保证 `cargo update` 不会把修复悄悄冲掉。

**2026-11-02 已从 0.6.1 升级到 0.10.0**，fork 的改动量从 167 行降到 **63 行 / 3 个文件**。
原来四个补丁里有两个上游自己修掉了，但 0.10.0 又带来一个新回归（下表 #5）：

| # | 问题 | 结果 |
|---|---|---|
| 1 | 入站 CC 发成了 `kLegacyMIDICCOutEvent`（那是**插件→宿主**的事件，VST3 没有入站 CC 事件，所以 CC 全被丢弃，踏板 CC64 对所有 VST3 乐器无效） | ✅ **升级后由上游提供**。0.9.0 的 `queue_mapped_midi_controller` 和我们思路一致但更完整：多了 `MidiMappingCache` 避免音频线程做 COM 调用，还手写了 VST3 3.8 的 `IMidiMapping2` ABI，pitch bend / aftertouch 也一并修了。**我们的补丁已删除。** |
| 2 | `setupProcessing` 之前就 `setActive(1)`，且硬编码 44100/512 | ✅ **升级后由上游提供**。机制不同：`load()` 签名没变，真实配置由 `set_audio_config()` 在 load 后传入，`start_processing` 对比 `applied_setup` 决定是否重新 setup。上游 changelog：Dexed 96 kHz 包络误差 48% → 1.2%。**我们的补丁已删除。** |
| 3 | `open_editor` 把 `getSize()` 失败当致命错误。Kontakt 的 QML 编辑器在 `attached()` 之前会失败，有些编辑器还会返回 `kResultOk` + 空 rect | ⚠️ **仍需本地补丁**（`plugin_impl.rs`）。改成非致命，保留 800×600 默认值 —— 和同文件 `get_editor_size()` 的既有行为一致，所以用 800×600 而不是原来的 1000×760。 |
| 4 | `PluginWindow::open` 会先创建临时 `IPlugView` 探测尺寸；Kontakt 只容忍一个 view，真正的 view 随后失败 | ⚠️ **仍需本地补丁**（`window.rs`）。去掉 open 路径上的 `get_editor_size()` 预查询，直接用 800×600 开窗，等 attach 之后由 `IPlugFrame::resizeView` 报告真实尺寸。 |
| 5 | **0.10.0 新引入的回归**：`module_info.rs` 把拿不到 `IPluginCompatibility` 元数据当成致命加载错误。Kontakt 8 会声明 compatibility class 但 `getCompatibilityJSON` 返回 `kResultFalse`(0x1)，于是插件直接加载失败 | ⚠️ **仍需本地补丁**（`module_info.rs`）。这份数据是可选的（只描述本二进制能替代哪些旧 plugin UID），所以改成记日志后返回空列表。上游 PR #16 在独立修同一个问题。 |

#3 / #4 上游都还没修，也没有对应的 issue / PR，可以回馈。上游仓库：
<https://github.com/HelgeSverre/rust-vst3-host>（MIT，edition 2021，MSRV 1.85，CI 要求
`cargo fmt` + `clippy -D warnings` + 回归测试 + Keep-a-Changelog 条目）。另见上游 PR #16，
是另一位贡献者在修 Kontakt 8 的 `moduleinfo.json` 加载问题。

升级时注意 0.9.0 的 breaking change：**插件没有映射的 controller 现在会被静默丢弃**
（仍然返回 `Ok(())`）。

---

## License

本项目代码：**MIT OR Apache-2.0**（见 [LICENSE-MIT](LICENSE-MIT) /
[LICENSE-APACHE](LICENSE-APACHE)，二者任选其一）。

`vendor/` 下是两个第三方 crate 的 fork，各自保留原许可与版权声明：

- `vendor/vst3-host` — fork of [`vst3-host`](https://github.com/HelgeSverre/rust-vst3-host)
  0.10.0，MIT，© Helge Sverre（见 `vendor/vst3-host/LICENSE`）
- `vendor/vst` — fork of [`vst-rs`](https://github.com/RustAudio/vst-rs) 0.4.0，MIT
  （见 `vendor/vst/LICENSE`）

Kontakt、Native Instruments、SeeMusic 等名称/商标归各自所有者；本项目不包含、不分发任何
Kontakt 或商业音色库。使用 Kontakt 需要你自行拥有有效授权。
