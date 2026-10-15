# Easy2GIF

把视频转成 GIF 的桌面程序。**Rust 做内核，C++/QML 只做交互。**

选中区间靠两条进度条上的可拖动竖线，也可以直接输入时间点；界面会实时显示
选区的总时长。导出完全由 Rust 内核完成，界面只负责把用户意图转发过去。

程序、CMake 目标和 QML 模块统一使用 `easy2gif`；界面品牌为 **Easy2GIF**。

## 不依赖任何系统插件

程序**不需要用户安装任何东西**。运行所需的全部内容都随程序打包：

| 组件 | 打包方式 |
| --- | --- |
| Qt 运行库与 QML 模块 | `windeployqt --qmldir` 复制到程序目录 |
| Qt 平台/图像/样式插件 | 同上，放在 `platforms/`、`imageformats/` 等子目录 |
| 视频引擎 | 静态编译的 `ffmpeg.exe` / `ffprobe.exe` 放在 `bin/` |
| MinGW 运行库 | `libgcc_s_seh-1.dll`、`libstdc++-6.dll`、`libwinpthread-1.dll` |

视频引擎是按**子进程**方式调用的，所以既不需要头文件也不需要导入库，更不需要
系统里预先装好 ffmpeg。

控件样式在 `main()` 里、`QGuiApplication` 构造**之前**就被钉死成 `Basic`，
因此界面外观不取决于目标机器上恰好装了什么样式插件——这也是能安全删掉
Fusion/Imagine/Material/Universal 等未用样式的原因。

## 架构

```
QML 界面            Main.qml / FrameSlider.qml     只画界面、发意图
   ↓  Q_PROPERTY / Q_INVOKABLE / 信号
C++ 桥接层          GifBridge.cpp                  只转发，不碰像素
   ↓  C ABI（include/gifcore.h）
Rust 内核           src/                          解析、解码、量化、编码 GIF
   ↓  子进程
ffmpeg / ffprobe    随程序打包
```

**所有数据都在 Rust 里处理。** C++ 层不推导任何媒体信息——连"起始帧是第几帧"
都是回读内核得到的，而不是自己算的。

### Rust 内核（`src/`）

不依赖任何第三方 crate，全部手写，因此可以完全离线构建：

- `ffmpeg/json.rs` — 极简 JSON 解析器，用来读 `ffprobe` 的输出
- `ffmpeg/probe.rs` — 媒体信息、帧率换算、旋转处理
- `ffmpeg/frames.rs` — 封面帧解码与胶片条缩略图
- `gif/quantize.rs` — Wu 色彩量化（3 维前缀和 + 方差分裂），再跑几轮 Lloyd 迭代
  （轮数由「编码质量」决定：质量 5 使用 3 轮）
- `gif/encode.rs` — GIF89a 写入器与 LZW 压缩
- `export.rs` — 解码 → 建调色板 → 索引 → 编码的完整管线；大小估计走的就是这条管线，
  只是把结果写进一个只数字节的接收器，再按总帧数外推（采样段的位置由
  `estimate_segments` 决定：均匀铺满整个选区）
- `ffi.rs` — 给 C++ 用的 C ABI（当前 ABI 版本 7）。导出参数打包成
  `GifCoreExportOptions` 由 `gif_export_ex` 接收（`NULL` 即全默认），
  界面的大小估计走 `gif_estimate_size`，它填一个 `GifCoreEstimate`
- `main.rs` — `forge` 入口：先跑内核自检，再拉起外壳（**不在上面这条数据链路里**，
  见"Rust 入口 `forge`"一节）

GIF 编码器是独立验证过的：测试用**自己写的解压器**回读，再用 **ffprobe /
ffmpeg 这种第三方解码器**核对帧数与字节数。

## 功能

- 圆角扁平界面，右上角可切换浅色／深色主题；首次启动默认浅色，后续记住上次选择
- 后台导入视频，显示进度条、阶段百分比和当前任务；失败时保留原视频
- 打开视频，读取分辨率、帧率、总时长、总帧数、编码、有无音轨
- 起始帧预览 + 整片胶片条时间轴
- **两条可拖动竖线**直接拖动，或拖动各自的进度条手柄
- **直接输入时间点**：`00:01.500`、`1:02`、`83.5`、`150f`（帧号）
- 实时显示**选区总时长**，以及起始帧、结束帧、输出帧数
- 导出设置：帧率、宽度、颜色数、抖动（无 / 有序 / Floyd–Steinberg）、速度、循环
- **导出前的大小估计**：帧率、宽度、颜色数、抖动、速度、选区……任何一项一改，后台线程
  立刻重新估算并在界面上给出"预计大小"。估计走的是**真实的编码管线**，只是落在一个只
  数字节的接收器里——把选区**均匀切成 8 段**、每段取 2 帧（默认共 16 帧）真正编一遍，
  再区分关键帧、段内变化和重复率外推；各采样段独立重置差分状态（首段贴住选区开头，末段正好收在末帧）。这样快慢不均的
  片段不会被"只取中段"骗到：一段前静后噪的素材，旧口径会低估到 1/80。整个过程在 worker
  线程里跑，**不会阻塞界面**；参数连续拖动时会取消过期估算、丢弃旧结果，只保留最新的一次
- **高级设置**（点击「高级设置」展开；内核修复后输出可能与旧版不同）：
  - **编码质量** 1–10：调色板 k-means 精修轮数，越高越准、越慢。5 就是以前的 3 轮
  - **有损压缩** 0–100：先把 RGB 量化到更粗的色阶再取色，体积明显变小，代价是色带
  - **抖动强度** 0–100：抖动幅度，100 为默认，0 关闭；有序使用 4×4 Bayer，Floyd 使用误差扩散
  - **复用重复帧**：相邻完全相同的帧合并，只在帧延时上累加，重复画面多时省得很多
  - **帧间透明优化** + **透明容差**：与实际已显示画布差异在容差内的像素标为透明（GCE 透明位 +
    DoNotDispose），只写变化部分；容差是每通道的松弛量（0–64）
  - 一键「恢复默认」
- **多线程导出**：调色板搜索与逐帧索引 + LZW 并行。线程数可以选"自动"或具体值；
  自动时取**逻辑核心数的一半**（核数在运行时读取），线程数只影响速度，
  同一版本下输出字节与单线程完全一致
- **直接指定保存位置**：填写保存文件夹（`浏览` 按钮定位）和文件名，
  悬停导出按钮可查看完整目标路径。内核在导出前先真正试写一次，位置不可写时直接把原因写在界面上——
  文件被别的程序打开、文件夹只读、目标其实是个文件，都会说明白，而不是只丢一句
  `拒绝访问`。写不进去时**导出按钮直接置灰**，不会白等一场导出
- 导出进度与阶段提示，可取消；导出失败不会留下半成品 `.gif.part`

## 内核修复（2026-10-15）

已处理审查中的 F01–F16，详情见 [修复记录](docs/kernel-fixes.md)；原始问题见
[内核检查报告](docs/kernel-review.md)。

- 预览按实际 PTS 定位，探测/预览/胶片条/导出统一视频流；VFR 的界面帧号仍是基于平均帧率的近似值。
- 延时使用累计时间分配；为兼容播放器采用至少 2 cs 的延时，必要时降低采样率以维持指定速度。结果时长是实际 GIF 播放时长。
- ABI 7 中 `dither_strength=0` 明确关闭抖动。Rust `Default` 和 `NULL` options 仍使用 100；C 调用方应显式设置 100 以选用默认强度。需同时重建内核与前端。
- `cargo test --offline` 会生成新回归测试所需的视频，必须能运行 FFmpeg、FFprobe 和 rustc；工具缺失会报失败。性能基准单独用 `cargo test --release --offline threading_speeds_up_a_large_export -- --ignored --nocapture` 运行。

本轮进一步复用编码线程、RGB/索引/抖动/LZW 缓冲，在压缩前跳过重复帧，透明优化只编码最小变化矩形。
显示结果与时序保持一致，ABI 仍为 7；开启透明优化时输出字节会变化。实测数据、复现命令与剩余工作见
[内核性能优化记录](docs/kernel-optimization.md)。

## 构建

### 在 CLion 里直接点 Run

仓库里已经带好了 CLion 需要的配置，三个运行配置任选：

- **`Easy2GIF（Rust 入口）`** —— 推荐用这个。它跑的是 Rust 二进制
  `src/main.rs`（`cargo run --offline --bin forge`）：**先把内核跑起来**做体检，
  **再把 C++/QML 外壳拉起来**并等它退出。详见下一节。
- **`Easy2GIF`** —— 直接跑 Qt 界面那个可执行文件
  （`.idea/runConfigurations/easy2gif.xml`）。
- **`Rust 内核测试`** —— 执行 `cargo test --offline`。

构件配置方面：`CMakePresets.json` 的 `mingw-release` 预设一口气指定了生成器、
MinGW 编译器、Qt 路径、ffmpeg 路径，以及构建内核需要的 `RUSTUP_HOME` /
`RUSTUP_TOOLCHAIN`；`.idea/workspace.xml` 里另外写了一个名为 `Release` 的
CMake profile，不用 preset 也能配上。

只有一处需要留意：CLion 的工具链必须是 **MinGW**（默认工具链若选了 Visual
Studio，`CMakeLists.txt` 会直接报错并提示换工具链）。原因是 Rust 内核编的是
`x86_64-pc-windows-gnu`，C++ 那一半必须用同一套 ABI。

### Rust 入口 `forge`

`src/main.rs` 是一个普通的 Rust 二进制，在 `Cargo.toml` 里声明为
`[[bin]] name = "forge"`。它做两件事，顺序固定：

1. **启动内核** —— 解析媒体引擎（优先用外壳旁边自带的那套 `bin/`，否则回退到
   `PATH`）、通过 `include/gifcore.h` 那套 C ABI 建一个内核句柄、让它自检工具
   并报告线程预算。加 `--probe <视频>` 还会真跑一次探测。
2. **拉起外壳** —— 找到 `easy2gif.exe`，启动，等它退出，把它的退出码当成自己的。

两者在编译期是同一个程序：外壳把这份内核作为静态库链了进去。所以这个入口是
"一条方便的、自带体检的入口"，而不是中间代理 —— 外壳跑的是它自己那份内核实例，
不会和这个进程通信。

认不出来的参数会原样转发给外壳：

```powershell
cargo run --bin forge -- --open clip.mp4 --export out.gif --threads 8
cargo run --bin forge -- --no-shell --probe clip.mp4   # 只跑内核自检
```

`forge` 自己认这几个参数：`--shell <路径>`、`--dest <文件夹>`、`--publish`、
`--publish-only`、`--no-publish`、`--probe <视频>`、`--no-shell`、`--help`。
`--` 之后的参数原样转发给外壳。

CMake 侧一次 `cargo build --release --offline --lib --bin forge` 同时产出静态库和
这个入口，并把 `forge.exe` 拷进 `b\bundle\`，所以它和外壳并排、可以直接运行
（见下面「运行位置」一节：放对位置才写得出去）。

### 命令行

```powershell
.\build.ps1
```

产物在 `b\bundle\`，整个文件夹可以直接拷走运行。要指定别的 Qt 或 ffmpeg：

```powershell
.\build.ps1 -QtRoot D:\Qt\6.10.2\mingw_64 `
            -FfmpegExe C:\tools\ffmpeg.exe `
            -FfprobeExe C:\tools\ffprobe.exe
```

也可以直接调 CMake（生成器必须用 `MinGW Makefiles`）：

```powershell
cmake --preset mingw-release
cmake --build --preset mingw-release-bundle
```

注意 `build.ps1` 与 CMake 都会**跳过包管理器留下的启动器壳（shim）**：那种
几百 KiB 的 `ffmpeg.exe` 只会按自身所在目录去找真正的程序，在本机能跑，
一旦拷进 bundle 换台机器就会报 "Cannot find file at …"。所以配置阶段会
警告、打包之后还会真的执行一次 `bin\ffmpeg.exe -version` 来验证它能否独立运行。

Rust 工具链装在仓库内的 `.rustup_local`（用户目录在此环境下不可写），
目标平台是 `stable-x86_64-pc-windows-gnu`——本机没有 MSVC 编译器，而 Qt 是
MinGW 版，两边必须一致。

### 生成发布压缩包

```powershell
.\package.ps1 -SharedMedia -SevenZip   # 构建精简版，同时生成 ZIP 和 7z
.\package.ps1 -SkipBuild              # 将当前 bundle 打成 ZIP
```

压缩包与 SHA256 文件保存在 `b\releases`，解压整个文件夹后运行 `easy2gif.exe`。
`-SharedMedia` 下载固定版本、校验 SHA256，使用 FFmpeg/FFprobe 共用的 DLL，
避免两份静态程序包含重复的编解码代码；依赖源码地址和许可证随包附带。
也可向 `build.ps1` 传入其他完整的静态或共享版媒体工具。7z 需要支持该格式的
Windows 自带 `tar.exe`；不加 `-SevenZip` 时只生成通用 ZIP。

脚本使用全新暂存目录和媒体文件清单，不会把旧构建残留、临时视频或日志混入包内。
文件名与 `BUILD.json` 记录源码提交；源码存在未提交变动时会标记 `dirty`。
体积对比及后续可优化方向见 [包体优化记录](docs/package-size.md)。

## 运行位置与完整性标签：一个很容易踩的坑

**写入能不能成功，取决于这个 exe **文件本身**带的完整性标签。**

Windows 对**低完整性**进程有一条强制策略：它只能往同样带**「低」完整性标签**的位置
写。普通目录没有标签，策略按 Medium 处理，低完整性进程写进去直接被拒，报的就是

```
拒绝访问。 (os error 5)
```

决定进程身份的是**镜像文件自己的标签**（启动它的 shell 只给出上限）。这台机器上实测，
每次只改一个变量：

| 条件 | 进程身份 | 程序 `--export D:\CODE\…` |
| --- | --- | --- |
| `whoami.exe` 复制进一个贴了 Low 的目录 | **低** `S-1-16-4096` | — |
| 同一份副本，只把它的标签改成 **M** | **中** `S-1-16-8192` | — |
| 构建目录贴了 Low，于是新生成的 exe 也继承 Low | **低** | **exit 3，0 字节**，`cannot create D:\CODE\…gif.part: 拒绝访问。 (os error 5)` |
| 把构建目录和这个文件都改回 **M** | **中** | **正常** |

由此得到三条规则，方向别记反：

1. **新文件继承所在目录的标签**，所以**构建目录一旦是 Low，生成的 exe 就是 Low**。
   因此 `b`、`target`、`.rustup_local`、部署目录全部保持普通（Medium）。
2. **低完整性的父进程会把子进程压回低完整性**：CLion 的 Run 拉起来的 `forge.exe` 是
   Low，它再拉起的 `easy2gif.exe` 也是 Low —— 哪怕 `easy2gif.exe` 自己贴了 M，
   导出照样被拒。
3. **低完整性进程改不了自己的标签**，它需要的那个权限正是沙箱抽掉的那个；所以
   「程序发现自己被拒 → 用 `icacls` 给自己升权 → 重启」这条路走不通（试过，代码已删）。
   而且 `icacls` 在被拒时**照样返回 0**，只打印 `Access is denied.` /
   `Successfully processed 0 files` —— **不能用退出码判断它干没干活**。

**要让导出正常工作**：从**资源管理器 / 开始菜单**启动程序，或者从一个**正常方式启动的
CLion**里 Run。构建也一样，要从正常的终端/CLion 里跑：受限终端会把整条工具链压成 Low，
那时它连这些 Medium 目录都写不进去。

构建的最后一步会自动把 4 个程序贴成 M（见 `CMakeLists.txt` 的「Integrity labels」一段）；
`relocate.ps1` 是给"文件夹被复制 / 移动过、标签丢了"时补的。

> 这一节前后写过两个**错误**版本：早先说是"exe 所在的那层文件夹决定身份"，后来改成
> "目录贴 Low、程序贴 M"。第二个正是让 CLion 的 Run 按钮失效的原因：Low 目录里生出来的
> exe 就是 Low。正确的说法是 —— exe 文件自己的标签决定它是什么身份，而**目录**的标签
> 决定**新生成的文件**继承成什么。

反向那条规则也是实测出来的：4 个程序都贴了 M，从一个受限（Low）终端里启动它们，程序
仍然自报 `完整性 低（沙箱）`、导出照样被拒。所以两句话要一起记：**文件标签**决定这个
exe 能到什么身份，**父进程的令牌**决定上限。

```powershell
icacls b\rust\release\forge.exe   # 必须是 Medium —— 它决定 CLion 的 Run 跑出来是什么身份
icacls b\bundle\easy2gif.exe      # 必须是 Medium
icacls b                          # 也必须是 Medium：(OI)(CI)(NW)，不是 Low
icacls target                     # 同理：cargo test 的测试程序在这里生成
```

`target` 也中招过一次，症状很隐蔽：测试程序出生就是 Low，于是三个用到 `%TEMP%` 的
单元测试失败，报的却是

```
write the blocker file: Os { code: 5, kind: PermissionDenied, message: "拒绝访问。" }
```

而其余测试照过（它们走 `scratch_dir()` 的回退链，落到了 exe 旁边的目录里）。
把 `target` 改回 Medium、重新 `cargo test` 就全绿 —— 所以看到"莫名其妙只有几个测试被拒"，
先去 `icacls target` 看一眼。

**注意：文件夹被复制或移动过之后标签会丢**（`relocate.ps1` 会重新贴好）。
排查时先看 `icacls <目录>` 里有没有 `Mandatory Label`、写的是 Low 还是 Medium，
再谈权限问题。

排除过的几种猜测，实测都不成立：

- **不是目标文件夹的权限问题。**它的 ACL 里 `17189` 是 `FullControl`，
  PowerShell 往里面写文件毫无问题。
- **不是杀毒软件或受控文件夹访问。**Windows Defender 处于停止状态。
- **不是 exe 自己的 ACL。**切掉继承链、只留 `FullControl` 也照样被拒。
- **不是被外层进程"传染"的。**换个全新登录令牌的任务计划、或用 WMI 启动，
  结果一样。
- **不是那条能力 ACE。**项目里新建的文件夹确实会继承
  `S-1-4-834417400-1017309768   DeleteSubdirectoriesAndFiles, Write, Delete, Synchronize`，
  容易看成元凶，但它是一条**授权**、不是拒绝；真正的分界线是完整性标签。

**怎么修，一句话：把构建目录变回普通（Medium）。**

- **导出被拒 / 界面上出现橙色「沙箱运行 · 导出受限」**：这个进程是低完整性。先分清是
  *文件* 的问题还是 *启动方式* 的问题 —— `icacls b\bundle\easy2gif.exe`。显示 Low 就重新
  构建一次（构建的最后一步会把 4 个程序贴回 M），或者手动改：
  `icacls b\bundle\easy2gif.exe /setintegritylevel M`（`forge.exe`、`b\rust\release\forge.exe`、
  `bin\ffmpeg.exe`、`bin\ffprobe.exe` 同理）。文件已经是 Medium 却还是被拒，那就是
  **启动它的那个进程是 Low**（受限终端/沙箱，或从那里启动的 CLion）：换成从资源管理器、
  开始菜单，或**正常方式启动的 CLion**里 Run。
- **编译被拒**（`error: 拒绝访问。 (os error 5) at path "…\b\rustXXXXXX"`、
  `Cannot create temporary file in …: Permission denied`）：跑一次 `relocate.ps1`，
  它会把 `b`、`target`、`.rustup_local` 和部署目录改回 Medium，并把 4 个程序贴成 M。
- **CLion 里没有任何相关设置**，也不需要 —— 但 **CLion 自己是被谁启动的很关键**：
  从受限终端里启动的 CLion，跑出来的一切都是 Low。

### `relocate.ps1`

重建并重新贴标签（部署目录默认放 `%LOCALAPPDATA%\easy2gif-build`，可用 `-Store` 改；
脚本可反复运行）：

```powershell
.\relocate.ps1
```

exe 的路径仍然是 `b\bundle\easy2gif.exe`，CLion 的 Run 和 `cmake --build` 都不用改。
`b\bundle` 是指向项目外的 **junction**（构建过程把前端产物直接写进去，所以它同时是
部署目录）；`target\release` 特意保持真实目录，因为 cargo 要往那里写自己的构建锁
（`.cargo-build-lock`），路径一旦穿过 junction 它就会拒绝。Rust 入口 `forge.exe`
因此由构建过程直接拷进 `b\bundle`，和外壳并排。

`publish.ps1` 是另一条路——把整个 bundle 复制到干净位置再运行：

```powershell
.\publish.ps1             # 复制到 %LOCALAPPDATA%\easy2gif（已经是最新就跳过）
.\publish.ps1 -Launch     # 复制完直接启动
.\publish.ps1 -Destination D:\Tools\easy2gif -Launch   # 换个位置
.\publish.ps1 -Force      # 强制重新复制
```

它只做复制和自检（会再执行一次 `bin\ffmpeg.exe -version`），不重新编译；目标
目录不允许落在项目目录里面，否则等于没搬。目标已经和 `b\bundle` 一致时直接跳过
复制，所以可以放心把它当成 Run 的一步。

## 暂存文件

内核转换过程中要落一些临时文件（ffmpeg 的 stdout/stderr、解码出来的原始帧），
它们放在一个"暂存目录"里。**这个目录不会假定 `%TEMP%` 一定可用** ——
被精简过的启动器、被策略锁死的临时目录、以服务身份运行等情况都可能建不了
目录，而旧写法在这种情况下只会抛出系统那一句 `拒绝访问。 (os error 5)`，
完全看不出问题出在哪。

现在的候选顺序是：

1. `GIFCORE_TEMP` 环境变量（显式指定优先）
2. `%TEMP%\gifcore-tmp`
3. **程序自己所在目录**下的 `gifcore-tmp`
4. `%LOCALAPPDATA%\easy2gif\tmp`
5. 当前工作目录下的 `gifcore-tmp`

每个候选都会**真的建一个文件试一下**，取第一个成功的。全部失败时，报错会把
试过的每个路径和各自的失败原因都列出来，并提示用 `GIFCORE_TEMP` 指定一个
可写位置 —— 所以"打开视频显示拒绝访问"这类问题现在能直接定位。
`forge` 的自检也会把最终选中的暂存目录打印出来。

临时文件用完即删（RAII），只有目录本身会留下。

## 测试

```powershell
$env:GIFCORE_FFMPEG  = 'C:\path\to\ffmpeg.exe'
$env:GIFCORE_FFPROBE = 'C:\path\to\ffprobe.exe'
cargo test --offline
```

单元测试覆盖 JSON、帧率换算、量化误差、LZW 往返；集成测试（`tests/pipeline.rs`）
会真的调用 ffmpeg 导出 GIF，再让 ffprobe/ffmpeg 独立回读验证。

## 命令行（便于自动化与排查）

```
easy2gif.exe --open <视频> [--screenshot <png>] [--export <gif>]
             [--delay <毫秒>] [--threads <0=自动|n>] [--log <文件>]
```

`--export` 走后端导出管线并在结束时退出，`--screenshot` 抓一张界面图后退出，
`--threads` 指定导出线程数（0 或省略即自动）。这几个开关只是把界面上的操作
在启动时自动做一遍，走的仍是同一条代码路径。

`--log` 把过程追加写进一个文本文件：引擎自检结果、视频有没有载入、导出的成败
与内核原话、截图结果。**排查时几乎必须要它**——这是 GUI 子系统程序，没有控制台，
`> out.txt 2>&1` 什么也抓不到（文件会是 0 字节），退出码是外面唯一能看到的信号：

```
2026-10-08T19:56:18 tools ready=1 (视频引擎就绪（内置，无需安装）)
2026-10-08T19:56:18 media loaded=1 error=
2026-10-08T19:56:18 export ok: 60 frames · 320x240 · 12.0 fps · 707.6 KB · 5.00 s clip
```

失败时最后一行就是原因，例如：

```
export FAILED: 保存失败：文件可能正被其他程序打开，或该文件夹没有写入权限。
cannot create D:\out\x.gif.part: 拒绝访问。 (os error 5)
```
