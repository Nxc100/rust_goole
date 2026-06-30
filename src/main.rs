//! google_img_translate
//!
//! 用浏览器自动化驱动 Google 翻译的「图片翻译」，批量翻译本地图片，按原名保存。
//! - 无命令行参数：启动图形界面（GUI）。
//! - 带命令行参数：命令行批处理模式。
//!
//! 原理与接口说明见 README.md。

// 发布版用 windows 子系统：双击启动 GUI 时不弹出黑色控制台窗口。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod gui;
mod translator;

use clap::Parser;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use translator::{Config, Progress};

#[derive(Parser, Debug)]
#[command(
    name = "google_img_translate",
    about = "用本地 Chrome 驱动 Google 图片翻译，批量翻译目录内图片并按原名保存到输出目录（无参数则启动图形界面）"
)]
struct Cli {
    /// 输入图片目录
    #[arg(short = 'i', long)]
    input: PathBuf,
    /// 输出目录
    #[arg(short = 'o', long)]
    output: PathBuf,
    /// 目标语言代码，例如 en / ja / zh-CN / de / fr / ko ...
    #[arg(short = 't', long, default_value = "en")]
    target: String,
    /// 源语言代码，默认 auto
    #[arg(short = 's', long, default_value = "auto")]
    source: String,
    /// 单张图片翻译超时（秒）
    #[arg(long, default_value_t = 90)]
    timeout: u64,
    /// 无头模式运行（默认显示浏览器窗口）
    #[arg(long, default_value_t = false)]
    headless: bool,
    /// Chrome 可执行文件路径（默认自动探测）
    #[arg(long)]
    chrome: Option<PathBuf>,
    /// 覆盖已存在的输出文件
    #[arg(long, default_value_t = false)]
    overwrite: bool,
    /// 只处理顶层目录、不递归子目录（默认递归并复刻子目录结构）
    #[arg(long, default_value_t = false)]
    no_recursive: bool,
}

fn main() {
    // 无参数 → 图形界面
    if std::env::args().len() <= 1 {
        if let Err(e) = gui::run_gui() {
            eprintln!("GUI 启动失败: {e}");
        }
        return;
    }

    // 有参数 → 命令行模式。发布版是 windows 子系统，需附加到父终端才能打印。
    attach_parent_console();

    let cli = Cli::parse();
    let cfg = Config {
        input: cli.input,
        output: cli.output,
        source: cli.source,
        target: cli.target,
        timeout: Duration::from_secs(cli.timeout),
        headless: cli.headless,
        overwrite: cli.overwrite,
        recursive: !cli.no_recursive,
        chrome: cli.chrome,
    };

    println!(
        "共开始：源语言={}，目标语言={}，输出={}",
        cfg.source,
        cfg.target,
        cfg.output.display()
    );

    let cancel = AtomicBool::new(false);
    translator::run_batch(&cfg, &cancel, print_progress);
}

fn print_progress(p: Progress) {
    match p {
        Progress::Total(t) => println!("共 {t} 张图片"),
        Progress::FileStart { idx, total, name } => {
            print!("[{}/{}] 翻译: {} ... ", idx + 1, total, name);
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
        Progress::FileOk { path, bytes, skipped, .. } => {
            if skipped {
                println!("跳过（已存在）");
            } else {
                println!("完成 -> {} ({} KB)", path.display(), bytes / 1024);
            }
        }
        Progress::FileErr { error, .. } => println!("失败: {error}"),
        Progress::Log(s) => println!("{s}"),
        Progress::Done { ok, failed } => println!("==== 完成：成功 {ok}，失败 {failed} ===="),
        Progress::Fatal(e) => eprintln!("致命错误：{e}"),
    }
}

/// 命令行模式下，把进程附加到启动它的终端，以便 println! 能显示。
#[cfg(windows)]
fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = 0xFFFF_FFFF;
    extern "system" {
        fn AttachConsole(dw_process_id: u32) -> i32;
    }
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

#[cfg(not(windows))]
fn attach_parent_console() {}
