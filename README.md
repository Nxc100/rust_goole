# google_img_translate

用本地 Chrome 浏览器自动化驱动 **Google 翻译 ·「图片翻译」** 功能，批量翻译本地目录里的图片，
按**原文件名**保存到指定输出目录。带**图形界面（GUI）**，也支持命令行。**完成一张立即保存一张。**

## 功能

1. 图形界面里指定**目标语言**、**输入图片目录**、**输出目录**（均为 Windows 本地路径）。
2. 对输入目录里的图片逐张做图片翻译（按指定语言），翻译后按原名存到输出目录。
3. 通过浏览器自动化完成，会打开本机 Chrome 浏览器执行。

支持的图片格式：`.jpg / .jpeg / .png / .webp`。

**支持多级子目录**：输入目录下可以有任意层级的子目录，工具会递归找出所有图片，
并在输出目录里**复刻完全相同的子目录结构**保存翻译结果（默认开启，可在界面关闭「包含子目录」）。
例如：

```
输入目录/                      输出目录/
├─ a.jpg              ──▶      ├─ a.jpg
├─ 详情/                       ├─ 详情/
│  ├─ 1.jpg                    │  ├─ 1.jpg
│  └─ 细节/2.jpg               │  └─ 细节/2.jpg
└─ 主图/3.png                  └─ 主图/3.png
```

## 快速开始（图形界面）

直接**双击** `google_img_translate.exe`（位于 `target\release\`）即可打开界面：

1. 点「输入目录 · 浏览…」选择待翻译图片所在文件夹；
2. 点「输出目录 · 浏览…」选择保存位置（不存在会自动创建）；
3. 选「目标语言」（默认英语）、必要时选「源语言」（默认自动检测）；
4. 点「▶ 开始翻译」。期间会弹出一个 Chrome 窗口自动操作，**请勿关闭它**；
5. 右侧实时显示每张图的状态（✔成功 / ×失败）、进度条、日志；翻译完点「📂 打开输出目录」查看。

> 界面交互：目录选择对话框、语言下拉框、进度条、逐文件状态列表、实时日志、停止、打开输出目录。

## 命令行用法（可选）

带参数运行即进入命令行批处理模式：

```powershell
# 用脚本（自动选 release/debug）
./run.ps1 -i "D:\图片\input" -o "D:\图片\out_en" -t en
# 翻译成日语，源语言固定为中文
./run.ps1 -i "D:\图片\input" -o "D:\图片\out_ja" -t ja -s zh-CN

# 或直接调用可执行文件
target\release\google_img_translate.exe -i "D:\图片\input" -o "D:\图片\out_en" -t en
```

| 参数 | 说明 | 默认 |
|---|---|---|
| `-i, --input <DIR>`  | 输入图片目录 | （必填） |
| `-o, --output <DIR>` | 输出目录（自动创建） | （必填） |
| `-t, --target <代码>` | 目标语言代码 | `en` |
| `-s, --source <代码>` | 源语言代码（`auto`=自动检测） | `auto` |
| `--timeout <秒>` | 单张图片超时 | `90` |
| `--headless` | 无头模式（默认显示窗口，更不易被反爬降级） | 关 |
| `--chrome <路径>` | 指定 Chrome 可执行文件 | 自动探测 |
| `--overwrite` | 覆盖已存在的输出文件（默认跳过） | 关 |
| `--no-recursive` | 只处理顶层、不递归子目录（默认递归并复刻子目录结构） | 关 |

### 环境变量（脚本化启动 GUI）

启动 GUI 前设置以下环境变量可预填界面，并可自动开始：

- `GIMG_INPUT` / `GIMG_OUTPUT`：输入/输出目录
- `GIMG_TARGET` / `GIMG_SOURCE`：目标/源语言代码（如 `en`、`ja`、`zh-CN`、`auto`）
- `GIMG_AUTOSTART=1`：启动后自动开始翻译

### 常用语言代码

`en` 英语 · `ja` 日语 · `ko` 韩语 · `zh-CN` 简体中文 · `zh-TW` 繁体中文 ·
`de` 德语 · `fr` 法语 · `es` 西班牙语 · `ru` 俄语 · `it` 意大利语 · `pt` 葡萄牙语 ·
`th` 泰语 · `vi` 越南语 · `ar` 阿拉伯语 · `id` 印尼语 · `hi` 印地语

## 构建（打包 exe）

```powershell
# 方式一：脚本（自动把 MinGW 加入 PATH，编译 release）
./build.ps1

# 方式二：手动
$env:PATH = "D:\xc_test\new_proj\rust_prjo\toolchain\mingw64\bin;$env:PATH"
cargo build --release
```

产物：单文件 **`target\release\google_img_translate.exe`**（约 27 MB）。
该 exe **自包含、可拷贝到其他 Windows 机器**直接运行（运行时不需要 MinGW；中文字体自动从系统加载；
需目标机器已装 Chrome）。

## 环境要求

- 本机已安装 **Google Chrome**（默认自动探测；也可在「高级设置」里指定路径）。
- 能正常访问 `translate.google.com`。
- **构建时**需要 GNU 工具链 + MinGW-w64（本机无 MSVC `link.exe`）：
  - Rust 工具链已固定为 `stable-x86_64-pc-windows-gnu`（见 `rust-toolchain.toml`）。
  - MinGW 的 `bin` 目录需在 PATH 中（`build.ps1` 已自动加入本机路径）。

## 原理简介

Google 图片翻译没有公开 API，其内部接口要求一个由浏览器端反爬脚本（botguard）为**每张图**
现场生成的令牌，纯 HTTP 无法绕过。因此本工具用浏览器自动化：

- 打开 `translate.google.com` 的图片翻译页（语种由 URL 的 `sl`/`tl` 参数控制）；
- 注入 XHR/fetch 钩子，捕获内部翻译 RPC（`GetImageTranslation` / `rpcid=WqWDPb`）的响应；
- 用 `DataTransfer` 以纯 JS 方式把本地图片放进文件输入框、触发翻译；
- 从捕获到的响应里解出翻译后图片（base64），解码后按原名落盘。

技术栈：[`eframe`/`egui`](https://crates.io/crates/eframe)（界面）+
[`headless_chrome`](https://crates.io/crates/headless_chrome)（驱动 Chrome，仅用 navigate/evaluate）+ `rfd`（原生文件对话框）。

## 注意事项

- **输出按原文件名保存**。Google 返回的翻译图通常是 JPEG，因此一张 `.png` 输入得到的输出文件名
  虽仍是 `.png`，但内容是 JPEG 编码（绝大多数看图/上传场景可正常使用）。
- 翻译时会弹出一个 Chrome 窗口（非无头），这是有意为之：可见的真实浏览器更不易被 Google
  反爬识别为自动化而**降级为"不翻译、原样返回"**。可关掉「显示浏览器窗口」走无头，但可能不翻译，请自测。
- 逐张串行处理，每张之间有短暂间隔以降低被限流概率。
- 默认**跳过**输出目录中已存在的同名文件；要重译勾选「覆盖已存在文件」。

## 源码结构

```
src/
├─ main.rs        # 入口：无参→GUI，有参→CLI
├─ gui.rs         # egui 图形界面
└─ translator.rs  # 核心翻译逻辑（GUI/CLI 共用）
```
