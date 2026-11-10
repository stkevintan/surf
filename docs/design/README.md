# Surf Studio UI 设计

> 状态：原型已评审（2026-11-04）。`prototype.html` 是可交互原型，直接用浏览器打开；`tokens.json` 是颜色、字体、尺寸的唯一来源。
> 本文描述 GPUI 实现要遵守的布局、视图、交互模型和扩展位。

## 1. 原则

- **结构参考 Logic Pro，皮肤是自己的。** 单窗口、区域可开关、底部编辑器可切换，沿用 Logic 的快捷键习惯；视觉是扁平、中等对比的 slate 深色主题。
- **扁平。** 不用渐变、浮雕、内阴影。状态靠「浅色底 + 1px 描边 + 前景色」表达，只有浮动菜单有投影。
- **留位不留坑。** 暂不实现的功能（Automation、Step Sequencer、Score 等）在界面上有固定位置，灰显并标注 planned。以后实现时不改布局。
- **一个 transport，多个视图。** 不再有 Live / Play 模式。MIDI 输入始终在线，录音是 transport 状态；所有视图读同一个 Project。

## 2. 窗口布局

```
┌──────────────────────────────────────────────────────────────────────────┐
│ Control bar 48   [区域开关][transport][LCD][count-in/metro][MIDI][CPU] … │
├──────────────────────────────────────────────────────────────────────────┤
│ Toolbar 32       planned… │ Export                                  │
├────────────┬────────────┬────────────────────────────────────────────────┤
│ Library    │ Inspector  │ Tracks 区 / Perform 视图                        │
│ 220        │ 236        │  pane header 30                                 │
│ (Y, 默认关)│ (I, 默认开)│  ┌ track headers 236 ┬ ruler 34 ───────────────┐│
│            │ Region ▾   │  │ lane 54 × N       │ regions                  ││
│ 搜索       │ Track  ▸   │  └───────────────────┴──────────────────────────┘│
│ 插件分类   │ ┌────┬────┐│ ═══ splitter 5（拖动调整编辑器高度）═══════════ │
│            │ │轨道│主输││ Editor（E/P/X，默认 Piano Roll，300 高，≥140） │
│ Rescan     │ │strip│出 ││  tabs 30: Mixer │ Piano Roll │ Event List │ …   │
│            │ └────┴────┘│  bar 32 + local inspector 156 + canvas          │
├────────────┴────────────┴────────────────────────────────────────────────┤
│ Status bar 24   音频设备 · plugin host 数 · 状态消息 · Undo: <label>      │
└──────────────────────────────────────────────────────────────────────────┘
```

所有尺寸见 `tokens.json` → `layout`。区域之间用 1px `line.default` 分隔，不留间距。

### 区域开关

| 区域 | 快捷键 | 默认 | 说明 |
|---|---|---|---|
| Library | `Y` | 关 | 插件浏览器，为**当前轨道**选乐器 |
| Inspector | `I` | 开 | Region / Track 参数（可折叠）+ 两条 channel strip |
| Toolbar | — | 开 | planned 功能 + 导出。编辑命令在系统菜单栏和 Tracks 标题栏 |
| Editor | `E` | 开 | 底部编辑器；`P` 直达 Piano Roll，`X` 直达 Mixer；可最大化或关闭 |
| Smart Controls | `B` | — | planned |

### 主区域：Tracks / Perform

pane header 右侧的分段控件切换。

- **Tracks**：track header（编号、图标、名字、乐器、M/S/R）+ 时间线。标题栏有工具、Snap、Quantize / Split / Join 图标（悬停显示名称和快捷键）、Zoom。

### 菜单栏

所有命令都在系统菜单栏：Surf、File（New ⌘N、Open ⌘O、Close ⌘W、Save ⌘S、Save As ⇧⌘S、Bounce WAV ⌘B、Export MIDI ⇧⌘E）、Edit、Track、Navigate、View、Window。菜单项旁显示快捷键。单字母快捷键只在 Musical Typing 关闭时生效。macOS 用系统菜单栏；Windows 和 Linux 上 GPUI 0.2 不画菜单栏，Surf 自己在窗口顶部画一条（26 高，`views::menu_bar`，和系统菜单同一份定义）。`SURF_IN_WINDOW_MENU=1` 可以在 Mac 上强制看自己画的那条。View 菜单里已打开的面板、当前编辑器标签和主视图打勾；Navigate 里的 Play、Metronome、Musical Typing 也按状态打勾。两种菜单栏读同一个 `menu::checked`，所以显示一致。
- **Perform**：落键视图，占满主区域。隐藏 Tracks 和所有只属于 Tracks 的控件，只留切换按钮。粒子和发光以后由 wgpu stage renderer 实现，这里只画平面音符块。

### 编辑器标签

| 标签 | 状态 | 内容 |
|---|---|---|
| Mixer | 有 | 每轨一条 strip + Stereo Out 靠右 |
| Piano Roll | 有 | 编辑 `editRegion`；左侧 local inspector（量化、强度、力度） |
| Event List | 有 | 当前 region 的音符表，与 Piano Roll 共享选择 |
| Step Sequencer | planned | |
| Score | planned | |

## 3. 组件

### Channel strip（Inspector 与 Mixer 共用）

从上到下：EQ 缩略图 → Setting → Instrument（内置绿色 / 插件紫色）→ Audio FX 槽（+ 空槽）→ Sends → Output → 声像旋钮 → dB / 峰值 → 推子 + 双声道电平表 → M / S → 轨道名（顶部 2px 轨道色）。

- 同一行的 strip 用占位槽对齐 FX 数量，保证旋钮和推子在同一高度。
- 推子曲线：`pos = ((dB + 60) / 66) ^ 1.6`，顶部 +6 dB，底部 −∞。
- 声像：竖向拖动，双击回中；范围 −64…+63。
- Instrument 槽点击打开 Library。

### Region

- 填充为轨道色 15% 透明度（选中 30%），描边 50%（选中 100%，1.5px）。
- 左上角大写名字（label 字体，轨道色），下面是音符缩略图。
- 静音轨道整体降到 45% 透明度。

### Piano Roll 音符

- 颜色按力度在 `color.velocity.stops` 间插值，和 Logic 一致；选中时向白色混 35%。
- 音符内部画一条长度和力度成比例的细线。
- 超出 region 结尾的音符降到 35% 透明度，不播放。

### 状态表达

| 状态 | 表达 |
|---|---|
| 开启 / 选中 | `accent.bg` 底 + `accent.line` 描边 + `accent.fg` 前景 |
| 播放 | `state.play` 实底 |
| 录音 | `state.record` 实底 |
| Mute / Solo | `state.mute` / `state.solo` 实底，深色字 |
| Planned | 40% 透明度，点击时在状态栏提示 |

## 4. 交互模型

这些规则直接影响 `surf-core` 的设计。

**选择与焦点。** 有两套独立的选择：Tracks 区选的是 region，编辑器里选的是 note。键盘命令（Delete、Copy/Cut/Paste、Quantize、Select All、Transpose）作用于最后被点击的区域（`focusArea`）。`editRegion` 是钢琴卷帘当前打开的 region，选中某个 region 时随之更新。

**撤销。** 所有修改项目的操作都可撤销，状态栏显示下一步撤销的名字（如 `Undo: Move Notes`）。拖拽类操作在移动超过 2–3px 时才提交一次，所以一次拖拽对应一步撤销。原型里用快照实现，正式版应该是 `surf-core` 的 command log。

**Tracks 区。**
- 工具：Pointer / Pencil（空白处新建一小节 region）/ Scissors（按 Snap 切开）/ Eraser。
- 拖 region 移动（可以跨轨），拖右边缘改长度，`Opt` + 拖动复制，空白处拖动框选，`Shift` 加选。
- 标尺上半部分拖动设置 Cycle，下半部分点击或拖动定位。
- `Cmd/Ctrl` + 滚轮缩放（以鼠标位置为中心），`Shift` + 滚轮横向滚动。
- 双击 region 打开 Piano Roll。双击轨道名重命名。

**Piano Roll。**
- 拖音符改位置和音高（按 Grid 吸附），拖右边缘改长度，`Opt` 复制，框选，`Shift` 加选。
- Pencil 点击加音符并可以直接拖出长度；双击空白处加音符，双击音符删除。
- 在力度条上竖向拖动改力度，多选时按相对量调整。
- 点左侧键盘试听；拖动时音高一变就试听一次。
- 滚轮上下滚音高，`Cmd/Ctrl` + 滚轮缩放。

**录音。** 按 `R` 打开 Musical Typing 并开始播放。录到的音符先实时画在一个红色临时 region 里，停止后生成 `Take N` region 并选中。录音期间 Cycle 不回绕。

**Transport。** 播放时按 Stop 停在当前位置，再按一次回到 Cycle 起点或 0。`,` / `.` 按小节后退或前进。

## 5. 快捷键

| 键 | 功能 | 键 | 功能 |
|---|---|---|---|
| `Space` | 播放 / 停止 | `R` | 录音 |
| `Return` | 回到开头 | `C` | Cycle |
| `,` `.` | 后退 / 前进一小节 | `K` | 节拍器 |
| `E` | 开关编辑器 | `P` | Piano Roll |
| `X` | Mixer | `I` / `Y` | Inspector / Library |
| `Q` | 量化 | `M` / `S` | 静音 / 独奏当前轨道 |
| `↑` `↓` | 切换轨道 | `Opt+↑/↓` | 移调 ±1（`Shift` ±12） |
| `Cmd+Z` / `Shift+Cmd+Z` | 撤销 / 重做 | `Cmd+A` | 全选 |
| `Cmd+T` | 在播放头处切开 | `Cmd+J` | 合并 region |
| `Cmd+K` | Musical Typing | `Delete` | 删除选中项 |
| `Cmd+S` / `Shift+Cmd+S` | 保存工程 / 另存为 | `Cmd+O` | 打开工程或 MIDI 文件 |
| `Cmd+N` | 新建工程 | `Cmd+W` | 关闭窗口 |
| `Cmd+B` / `Shift+Cmd+E` | Bounce WAV / 导出 MIDI | `Opt+Cmd+N` / `Cmd+Delete` | 新建 / 删除轨道 |
| `Cmd+C` / `Cmd+X` | 复制 / 剪切 | `Cmd+V` | 粘贴：音符贴在原音符后一格，region 贴到播放头 |

Musical Typing 开启时，`A W S E D F T G Y H U J K O L P ;` 弹 C3 到 E4（Logic 规则：C3 = MIDI 60），`Z` / `X` 换八度。这时字母键不再触发上表中的快捷键。

## 6. 扩展位（planned）

| 位置 | 功能 |
|---|---|
| Control bar | Smart Controls、List Editors、Notepad、Loop Browser |
| Toolbar | Automation、Bounce in Place、Smart Tempo、Markers、Video export |
| Tracks 区 | Global Tracks（tempo / signature / markers），在左上角 `GLOBAL ▸` |
| 编辑器 | Step Sequencer、Score 标签 |
| Piano Roll local inspector | Scale Quantize、Articulation |
| Channel strip | Audio FX、Sends（槽位已画出，暂不承载插件） |
| Library | CLAP 分类 |
| LCD | 双击直接编辑数值 |

## 7. 落到代码

- **Token。** `crates/surf-ui/src/theme.rs` 由 `python3 tools/gen_theme.py` 从 `tokens.json` 生成，不要手改。所有颜色和尺寸都从这里取，视图代码里不出现字面量颜色。以后要加浅色主题，再出一份同结构的 token 就行。
- **字体。** IBM Plex Sans / Mono 和 Barlow Condensed 都是 OFL 授权，已打包在 `crates/surf-ui/assets/fonts/`（附许可证），启动时通过 `surf_ui::fonts::register` 注册，离线也一致。原型仍从 Google Fonts 加载。
- **GPUI 分工。** 窗口外框、Inspector、Mixer、Library、各种表单控件用 GPUI 元素；Tracks 时间线、Piano Roll、Perform 用 `canvas` 自绘，因为数量多而且命中检测是自己做的。
- **数据。** 视图只读 `surf-core::Project`，修改都走 command（同时也是撤销记录）。selection、focusArea、editRegion 放在 UI state 里，不进 Project 文件。
- **Plugin host。** Instrument 槽显示的「helper running / in process」来自 `surf-plugin` 客户端的状态。内置音色在主进程里跑，VST 每个实例占一个 helper 进程。

## 8. 原型和真实实现的差异

- 声音是浏览器里的正弦合成，插件、导出、编辑器窗口这些操作只在状态栏打一条消息。
- 状态栏和 Instrument 槽里的设备名、pid、CPU 占用都是写死的假数据。
- 撤销用 JSON 快照实现，没有合并相邻操作。
- 画布字体在 Google Fonts 加载完之前会先用系统字体渲染。
