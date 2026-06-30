//! egui 图形界面

use crate::translator::{self, Config, Progress, TARGET_LANGS};
use eframe::egui;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

pub fn run_gui() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([760.0, 680.0])
            .with_min_inner_size([640.0, 520.0])
            .with_title("Google 图片翻译 · 批量"),
        ..Default::default()
    };
    eframe::run_native(
        "Google 图片翻译 · 批量",
        options,
        Box::new(|cc| {
            install_cjk_font(&cc.egui_ctx);
            Ok(Box::new(App::default()) as Box<dyn eframe::App>)
        }),
    )
}

/// 加载系统中文字体，避免中文显示为方块
fn install_cjk_font(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑
        r"C:\Windows\Fonts\simhei.ttf", // 黑体
        r"C:\Windows\Fonts\Deng.ttf",   // 等线
        r"C:\Windows\Fonts\simsun.ttc", // 宋体
        r"C:\Windows\Fonts\msyh.ttf",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("cjk".to_owned(), egui::FontData::from_owned(bytes).into());
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "cjk".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("cjk".to_owned());
            ctx.set_fonts(fonts);
            return;
        }
    }
}

#[derive(Clone)]
enum Status {
    Waiting,
    Running,
    Skipped,
    Ok(String),
    Failed(String),
}

struct Row {
    name: String,
    status: Status,
}

struct App {
    input: Option<PathBuf>,
    output: Option<PathBuf>,
    target_idx: usize,
    source_idx: usize,
    show_window: bool, // 显示浏览器窗口（= !headless）
    overwrite: bool,
    recursive: bool, // 含子目录并复刻目录结构
    timeout_secs: u32,
    chrome_path: String,
    show_advanced: bool,

    autostart: bool,
    started_once: bool,
    running: bool,
    closing: bool,
    cancel: Arc<AtomicBool>,
    rx: Option<Receiver<Progress>>,

    total: usize,
    done: usize,
    ok: usize,
    failed: usize,
    current: String,
    rows: Vec<Row>,
    logs: Vec<String>,
    finished_msg: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        // 支持用环境变量预填，便于脚本化启动：
        //   GIMG_INPUT / GIMG_OUTPUT  输入/输出目录
        //   GIMG_TARGET / GIMG_SOURCE 目标/源语言代码（如 en, ja, zh-CN, auto）
        //   GIMG_AUTOSTART=1          启动后自动开始翻译
        let env_path = |k: &str| std::env::var(k).ok().map(PathBuf::from);
        let target_idx = std::env::var("GIMG_TARGET")
            .ok()
            .and_then(|c| TARGET_LANGS.iter().position(|(code, _)| *code == c))
            .unwrap_or(0);
        let source_idx = std::env::var("GIMG_SOURCE")
            .ok()
            .and_then(|c| translator::source_langs().iter().position(|(code, _)| *code == c))
            .unwrap_or(0);
        let autostart = std::env::var("GIMG_AUTOSTART").map(|v| v == "1").unwrap_or(false);
        Self {
            input: env_path("GIMG_INPUT").filter(|p| p.is_dir()),
            output: env_path("GIMG_OUTPUT"),
            target_idx,
            source_idx,
            show_window: true,
            overwrite: false,
            recursive: true,
            timeout_secs: 90,
            chrome_path: String::new(),
            show_advanced: false,
            autostart,
            started_once: false,
            running: false,
            closing: false,
            cancel: Arc::new(AtomicBool::new(false)),
            rx: None,
            total: 0,
            done: 0,
            ok: 0,
            failed: 0,
            current: String::new(),
            rows: Vec::new(),
            logs: Vec::new(),
            finished_msg: None,
        }
    }
}

impl App {
    fn source_langs(&self) -> Vec<(&'static str, &'static str)> {
        translator::source_langs()
    }

    fn start(&mut self, ctx: &egui::Context) {
        let (input, output) = match (&self.input, &self.output) {
            (Some(i), Some(o)) => (i.clone(), o.clone()),
            _ => {
                self.logs.push("请先选择输入目录和输出目录。".into());
                return;
            }
        };

        // 预先收集图片用于列表展示（与 worker 内排序一致）
        let imgs = translator::collect_images(&input, self.recursive).unwrap_or_default();
        self.rows = imgs
            .iter()
            .map(|p| Row {
                name: translator::rel_display(&input, p),
                status: Status::Waiting,
            })
            .collect();

        self.total = self.rows.len();
        self.done = 0;
        self.ok = 0;
        self.failed = 0;
        self.current.clear();
        self.logs.clear();
        self.finished_msg = None;
        self.cancel = Arc::new(AtomicBool::new(false));

        let sources = self.source_langs();
        let cfg = Config {
            input,
            output,
            source: sources[self.source_idx].0.to_string(),
            target: TARGET_LANGS[self.target_idx].0.to_string(),
            timeout: Duration::from_secs(self.timeout_secs.max(10) as u64),
            headless: !self.show_window,
            overwrite: self.overwrite,
            recursive: self.recursive,
            chrome: {
                let t = self.chrome_path.trim();
                if t.is_empty() { None } else { Some(PathBuf::from(t)) }
            },
        };

        let (tx, rx): (Sender<Progress>, Receiver<Progress>) = std::sync::mpsc::channel();
        self.rx = Some(rx);
        self.running = true;

        let cancel = self.cancel.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            translator::run_batch(&cfg, &cancel, |p| {
                let _ = tx.send(p);
                ctx2.request_repaint();
            });
        });
    }

    fn drain(&mut self) {
        let mut msgs = Vec::new();
        if let Some(rx) = &self.rx {
            while let Ok(p) = rx.try_recv() {
                msgs.push(p);
            }
        }
        for p in msgs {
            match p {
                Progress::Total(t) => self.total = t,
                Progress::FileStart { idx, total, name } => {
                    self.total = total;
                    self.current = name.clone();
                    if let Some(r) = self.rows.get_mut(idx) {
                        r.status = Status::Running;
                    }
                    self.logs.push(format!("[{}/{}] 翻译: {}", idx + 1, total, name));
                }
                Progress::FileOk { idx, name, path, bytes, skipped } => {
                    self.done += 1;
                    self.ok += 1;
                    if let Some(r) = self.rows.get_mut(idx) {
                        r.status = if skipped {
                            Status::Skipped
                        } else {
                            Status::Ok(format!("{} KB", bytes / 1024))
                        };
                    }
                    if skipped {
                        self.logs.push(format!("  跳过（已存在）: {name}"));
                    } else {
                        self.logs.push(format!("  完成: {} ({} KB)", path.display(), bytes / 1024));
                    }
                }
                Progress::FileErr { idx, name, error } => {
                    self.done += 1;
                    self.failed += 1;
                    if let Some(r) = self.rows.get_mut(idx) {
                        r.status = Status::Failed(error.clone());
                    }
                    self.logs.push(format!("  失败: {name} — {error}"));
                }
                Progress::Log(s) => self.logs.push(s),
                Progress::Done { ok, failed } => {
                    self.running = false;
                    self.current.clear();
                    self.finished_msg = Some(format!("完成：成功 {ok}，失败 {failed}（共 {}）", self.total));
                    self.logs.push(format!("==== 完成：成功 {ok}，失败 {failed} ===="));
                }
                Progress::Fatal(e) => {
                    self.running = false;
                    self.current.clear();
                    self.finished_msg = Some(format!("出错：{e}"));
                    self.logs.push(format!("致命错误：{e}"));
                }
            }
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain();

        // 自动开始（环境变量 GIMG_AUTOSTART=1）
        if self.autostart
            && !self.started_once
            && !self.running
            && self.input.is_some()
            && self.output.is_some()
        {
            self.started_once = true;
            self.start(ctx);
        }

        // 关闭窗口时：若正在翻译，先停止 worker（让它收尾、关闭其 Chrome）再退出，
        // 避免强制退出导致 Chrome 子进程变孤儿。
        if ctx.input(|i| i.viewport().close_requested()) && self.running {
            self.cancel.store(true, Ordering::Relaxed);
            self.closing = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.closing && !self.running {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }

        if self.running {
            ctx.request_repaint_after(Duration::from_millis(120));
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(4.0);
            ui.heading("Google 图片翻译 · 批量工具");
            ui.label(
                egui::RichText::new("用本机 Chrome 自动翻译目录内图片，按原名保存到输出目录")
                    .small()
                    .weak(),
            );
            ui.separator();

            let enabled = !self.running;

            // ---- 目录选择 ----
            ui.add_enabled_ui(enabled, |ui| {
                egui::Grid::new("dirs")
                    .num_columns(3)
                    .spacing([8.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("输入目录");
                        let in_txt = self
                            .input
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "（未选择）".into());
                        ui.add(egui::Label::new(in_txt).truncate());
                        if ui.button("浏览…").clicked() {
                            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                self.input = Some(p);
                            }
                        }
                        ui.end_row();

                        ui.label("输出目录");
                        let out_txt = self
                            .output
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "（未选择）".into());
                        ui.add(egui::Label::new(out_txt).truncate());
                        if ui.button("浏览…").clicked() {
                            if let Some(p) = rfd::FileDialog::new().pick_folder() {
                                self.output = Some(p);
                            }
                        }
                        ui.end_row();
                    });

                ui.add_space(4.0);

                // ---- 语言选择 ----
                ui.horizontal(|ui| {
                    egui::ComboBox::from_label("目标语言")
                        .selected_text(TARGET_LANGS[self.target_idx].1)
                        .show_ui(ui, |ui| {
                            for (i, (_c, name)) in TARGET_LANGS.iter().enumerate() {
                                ui.selectable_value(&mut self.target_idx, i, *name);
                            }
                        });
                    ui.add_space(16.0);
                    let sources = self.source_langs();
                    egui::ComboBox::from_label("源语言")
                        .selected_text(sources[self.source_idx].1)
                        .show_ui(ui, |ui| {
                            for (i, (_c, name)) in sources.iter().enumerate() {
                                ui.selectable_value(&mut self.source_idx, i, *name);
                            }
                        });
                });

                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.show_window, "显示浏览器窗口（推荐，更稳定）");
                    ui.add_space(12.0);
                    ui.checkbox(&mut self.overwrite, "覆盖已存在文件");
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut self.recursive, "包含子目录（在输出目录复刻相同的目录结构）");
                });

                // ---- 高级 ----
                egui::CollapsingHeader::new("高级设置")
                    .open(Some(self.show_advanced))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label("单张超时(秒)");
                            ui.add(egui::DragValue::new(&mut self.timeout_secs).range(10..=600));
                        });
                        ui.horizontal(|ui| {
                            ui.label("Chrome 路径");
                            ui.add(
                                egui::TextEdit::singleline(&mut self.chrome_path)
                                    .hint_text("留空=自动探测")
                                    .desired_width(420.0),
                            );
                        });
                    });
            });

            ui.add_space(6.0);

            // ---- 操作按钮 ----
            ui.horizontal(|ui| {
                let can_start = !self.running && self.input.is_some() && self.output.is_some();
                if ui
                    .add_enabled(can_start, egui::Button::new("▶ 开始翻译"))
                    .clicked()
                {
                    self.start(ctx);
                }
                if self.running {
                    if ui.button("■ 停止").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                        self.logs.push("正在停止…（当前图片完成后停止）".into());
                    }
                }
                if let Some(out) = self.output.clone() {
                    if ui.button("📂 打开输出目录").clicked() {
                        let _ = std::process::Command::new("explorer").arg(out).spawn();
                    }
                }
            });

            ui.add_space(6.0);

            // ---- 进度 ----
            let frac = if self.total > 0 {
                self.done as f32 / self.total as f32
            } else {
                0.0
            };
            let bar_text = if self.running {
                format!("{}/{}  {}", self.done, self.total, self.current)
            } else if let Some(m) = &self.finished_msg {
                m.clone()
            } else {
                format!("{}/{}", self.done, self.total)
            };
            ui.add(egui::ProgressBar::new(frac).text(bar_text));
            ui.horizontal(|ui| {
                ui.colored_label(egui::Color32::from_rgb(40, 160, 70), format!("成功 {}", self.ok));
                ui.colored_label(egui::Color32::from_rgb(200, 60, 60), format!("失败 {}", self.failed));
                ui.label(egui::RichText::new(format!("共 {}", self.total)).weak());
            });

            ui.separator();

            // ---- 文件列表 + 日志（左右分栏）----
            let avail_h = ui.available_height().max(120.0);
            ui.columns(2, |cols| {
                cols[0].label(egui::RichText::new("文件状态").strong());
                egui::ScrollArea::vertical()
                    .id_salt("rows")
                    .max_height(avail_h - 24.0)
                    .auto_shrink([false, false])
                    .show(&mut cols[0], |ui| {
                        for r in &self.rows {
                            ui.horizontal(|ui| {
                                let (icon, color) = match &r.status {
                                    Status::Waiting => ("•", egui::Color32::GRAY),
                                    Status::Running => ("⏳", egui::Color32::from_rgb(220, 160, 40)),
                                    Status::Skipped => ("↪", egui::Color32::GRAY),
                                    Status::Ok(_) => ("✔", egui::Color32::from_rgb(40, 160, 70)),
                                    Status::Failed(_) => ("×", egui::Color32::from_rgb(200, 60, 60)),
                                };
                                ui.colored_label(color, icon);
                                ui.add(egui::Label::new(&r.name).truncate());
                                match &r.status {
                                    Status::Ok(info) => {
                                        ui.label(egui::RichText::new(info).weak());
                                    }
                                    Status::Skipped => {
                                        ui.label(egui::RichText::new("已存在").weak());
                                    }
                                    Status::Failed(e) => {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(200, 60, 60),
                                            egui::RichText::new(e).small(),
                                        );
                                    }
                                    _ => {}
                                }
                            });
                        }
                    });

                cols[1].label(egui::RichText::new("日志").strong());
                egui::ScrollArea::vertical()
                    .id_salt("logs")
                    .max_height(avail_h - 24.0)
                    .auto_shrink([false, false])
                    .stick_to_bottom(true)
                    .show(&mut cols[1], |ui| {
                        for line in &self.logs {
                            ui.label(egui::RichText::new(line).monospace().small());
                        }
                    });
            });
        });
    }
}
