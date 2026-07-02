//! 核心翻译逻辑：用 headless_chrome 驱动 Google 图片翻译。
//! GUI 与 CLI 共用。run_batch 通过回调上报进度，便于 GUI 实时刷新。

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use headless_chrome::{Browser, LaunchOptions, Tab};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

pub const SUPPORTED_EXT: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// 目标语言列表：(代码, 显示名)
// 显示名只用「中文 + 拉丁/西里尔」，避免系统中文字体缺韩/泰/阿拉伯/印地字形而显示为方块。
pub const TARGET_LANGS: &[(&str, &str)] = &[
    ("en", "英语 English"),
    ("ja", "日语 Japanese"),
    ("ko", "韩语 Korean"),
    ("zh-CN", "简体中文"),
    ("zh-TW", "繁体中文"),
    ("de", "德语 Deutsch"),
    ("fr", "法语 Français"),
    ("es", "西班牙语 Español"),
    ("ru", "俄语 Русский"),
    ("it", "意大利语 Italiano"),
    ("pt", "葡萄牙语 Português"),
    ("th", "泰语 Thai"),
    ("vi", "越南语 Vietnamese"),
    ("ar", "阿拉伯语 Arabic"),
    ("id", "印尼语 Indonesian"),
    ("hi", "印地语 Hindi"),
];

/// 源语言列表：auto + 目标语言列表
pub fn source_langs() -> Vec<(&'static str, &'static str)> {
    let mut v = vec![("auto", "自动检测 Auto")];
    v.extend_from_slice(TARGET_LANGS);
    v
}

#[derive(Clone)]
pub struct Config {
    pub input: PathBuf,
    pub output: PathBuf,
    pub source: String,
    pub target: String,
    pub timeout: Duration,
    pub headless: bool,
    pub overwrite: bool,
    pub chrome: Option<PathBuf>,
    /// 递归处理子目录，并在输出目录复刻同样的子目录结构
    pub recursive: bool,
    /// 并行浏览器数（高效模式）。1=串行单浏览器；>1=多开浏览器并行。
    pub concurrency: usize,
}

/// 进度事件，上报给 UI / 控制台
pub enum Progress {
    Total(usize),
    FileStart { idx: usize, total: usize, name: String },
    FileOk { idx: usize, name: String, path: PathBuf, bytes: usize, skipped: bool },
    FileErr { idx: usize, name: String, error: String },
    Log(String),
    Done { ok: usize, failed: usize },
    Fatal(String),
}

const UPLOAD_TMPL: &str = r#"(async () => {
  try {
    window.__caps = [];
    if (!window.__hooked) {
      window.__hooked = true;
      const oOpen = XMLHttpRequest.prototype.open, oSend = XMLHttpRequest.prototype.send;
      XMLHttpRequest.prototype.open = function(m,u){ this.__u=u; return oOpen.apply(this,arguments); };
      XMLHttpRequest.prototype.send = function(){
        this.addEventListener('load', function(){
          try { if (this.__u && this.__u.indexOf('batchexecute')>=0 && this.__u.indexOf('WqWDPb')>=0) window.__caps.push(this.responseText); } catch(e){}
        });
        return oSend.apply(this,arguments);
      };
      const oF = window.fetch;
      window.fetch = function(){
        const a=arguments; const u=(a[0]&&a[0].url)||a[0]; const p=oF.apply(this,a);
        try { if (typeof u==='string' && u.indexOf('batchexecute')>=0) p.then(r=>{ r.clone().text().then(t=>{ if (t.indexOf('WqWDPb')>=0) window.__caps.push(t); }); }); } catch(e){}
        return p;
      };
    }
    const input = document.querySelector('input[type=file][accept*="image"]');
    if (!input) return 'ERR:no-image-input';
    const b64 = "__B64__";
    const bin = atob(b64); const arr = new Uint8Array(bin.length);
    for (let i=0;i<bin.length;i++) arr[i]=bin.charCodeAt(i);
    const file = new File([arr], "image", {type:"__MIME__"});
    const dt = new DataTransfer(); dt.items.add(file);
    input.files = dt.files;
    input.dispatchEvent(new Event('change', {bubbles:true}));
    return 'OK:'+input.files.length;
  } catch(e) { return 'ERR:'+(e&&e.message||e); }
})()"#;

const EXTRACT_JS: &str = r#"(async () => {
  try {
    if (!window.__caps || !window.__caps.length) return 'PENDING';
    const t = window.__caps[0];
    const idx = t.indexOf('[["wrb.fr","WqWDPb"');
    if (idx < 0) return 'PENDING';
    let depth=0,inStr=false,esc=false,end=-1;
    for (let j=idx;j<t.length;j++){const c=t[j];if(esc){esc=false;continue;}if(c==='\\'){esc=true;continue;}if(c==='"'){inStr=!inStr;continue;}if(inStr)continue;if(c==='[')depth++;else if(c===']'){depth--;if(depth===0){end=j+1;break;}}}
    if (end<0) return 'PENDING';
    const env = JSON.parse(t.slice(idx,end));
    const data = JSON.parse(env[0][2]);
    if (!data[0] || !data[0][0]) return 'EMPTY';
    return 'B64:'+data[0][0];
  } catch(e) { return 'ERRX:'+(e&&e.message||e); }
})()"#;

const INPUT_PRESENT_JS: &str =
    r#"(document.querySelector('input[type=file][accept*="image"]') ? 'YES' : 'NO')"#;

fn is_supported(p: &Path) -> bool {
    p.extension()
        .and_then(OsStr::to_str)
        .map(|e| SUPPORTED_EXT.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn walk(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let p = entry?.path();
        if p.is_dir() {
            if recursive {
                walk(&p, recursive, out)?;
            }
        } else if p.is_file() && is_supported(&p) {
            out.push(p);
        }
    }
    Ok(())
}

/// 收集输入目录里支持的图片（recursive=true 时含所有子目录），按路径排序。
pub fn collect_images(input: &Path, recursive: bool) -> Result<Vec<PathBuf>> {
    let mut images: Vec<PathBuf> = Vec::new();
    walk(input, recursive, &mut images)?;
    images.sort();
    Ok(images)
}

/// 计算图片相对输入根目录的展示路径（用于界面/日志与输出目录复刻）。
pub fn rel_display(input: &Path, img: &Path) -> String {
    img.strip_prefix(input)
        .unwrap_or(img)
        .to_string_lossy()
        .to_string()
}

/// 批量翻译。on 为进度回调；cancel 置 true 时在图片之间停止。
pub fn run_batch(cfg: &Config, cancel: &AtomicBool, mut on: impl FnMut(Progress)) {
    if !cfg.input.is_dir() {
        on(Progress::Fatal(format!("输入目录不存在: {}", cfg.input.display())));
        return;
    }
    if let Err(e) = fs::create_dir_all(&cfg.output) {
        on(Progress::Fatal(format!("无法创建输出目录: {e}")));
        return;
    }
    let images = match collect_images(&cfg.input, cfg.recursive) {
        Ok(v) => v,
        Err(e) => {
            on(Progress::Fatal(format!("读取输入目录失败: {e}")));
            return;
        }
    };
    let total = images.len();
    on(Progress::Total(total));
    if total == 0 {
        on(Progress::Log("输入目录里没有支持的图片（jpg/jpeg/png/webp）。".into()));
        on(Progress::Done { ok: 0, failed: 0 });
        return;
    }

    let conc = cfg.concurrency.clamp(1, 8).min(total);
    on(Progress::Log(format!(
        "启动 {} 个浏览器（{}）...",
        conc,
        if cfg.headless { "无头" } else { "可见窗口" }
    )));

    // 启动 conc 个独立浏览器实例（高效模式下并行使用）
    let mut browsers: Vec<Browser> = Vec::new();
    for k in 0..conc {
        match launch_browser(cfg) {
            Ok(b) => browsers.push(b),
            Err(e) => {
                on(Progress::Fatal(format!("启动第 {} 个 Chrome 失败：{e}", k + 1)));
                return; // 已启动的浏览器在此 drop 关闭
            }
        }
    }

    let images_ref: &[PathBuf] = &images;
    let next = AtomicUsize::new(0);
    let next_ref = &next;
    let (tx, rx) = std::sync::mpsc::channel::<Progress>();

    let mut ok = 0usize;
    let mut failed = 0usize;

    // 并发：每个浏览器一个 worker 线程，从共享原子计数取下一张图；
    // 主线程异步收集各 worker 的进度并回调上报。
    std::thread::scope(|s| {
        for browser in browsers {
            let txc = tx.clone();
            s.spawn(move || worker(browser, images_ref, next_ref, cfg, cancel, txc));
        }
        drop(tx); // 关闭原始发送端：所有 worker 结束后 rx.recv 返回 Err，退出收集循环

        while let Ok(p) = rx.recv() {
            match &p {
                Progress::FileOk { .. } => ok += 1,
                Progress::FileErr { .. } => failed += 1,
                _ => {}
            }
            on(p);
        }
    });

    on(Progress::Done { ok, failed });
}

/// 单个 worker：独占一个浏览器，循环从共享队列取图翻译，完成后关闭浏览器。
fn worker(
    browser: Browser,
    images: &[PathBuf],
    next: &AtomicUsize,
    cfg: &Config,
    cancel: &AtomicBool,
    tx: Sender<Progress>,
) {
    let tab = match browser.new_tab() {
        Ok(t) => t,
        Err(e) => {
            let _ = tx.send(Progress::Log(format!("创建标签页失败：{e}")));
            return;
        }
    };
    let total = images.len();
    loop {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let i = next.fetch_add(1, Ordering::Relaxed);
        if i >= total {
            break;
        }
        let img = &images[i];
        let rel = img.strip_prefix(&cfg.input).unwrap_or(img.as_path());
        let name = rel.to_string_lossy().to_string();
        let _ = tx.send(Progress::FileStart { idx: i, total, name: name.clone() });

        let out_path = cfg.output.join(rel);
        if out_path.exists() && !cfg.overwrite {
            let _ = tx.send(Progress::FileOk { idx: i, name, path: out_path, bytes: 0, skipped: true });
            continue;
        }
        // 确保输出子目录存在
        if let Some(parent) = out_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                let _ = tx.send(Progress::FileErr { idx: i, name, error: format!("创建输出子目录失败: {e}") });
                continue;
            }
        }

        match translate_with_retry(&tab, cfg, img, 1) {
            Ok(bytes) => match fs::write(&out_path, &bytes) {
                Ok(_) => {
                    let _ = tx.send(Progress::FileOk { idx: i, name, path: out_path, bytes: bytes.len(), skipped: false });
                }
                Err(e) => {
                    let _ = tx.send(Progress::FileErr { idx: i, name, error: format!("保存失败: {e}") });
                }
            },
            Err(e) => {
                let _ = tx.send(Progress::FileErr { idx: i, name, error: format!("{e}") });
            }
        }

        std::thread::sleep(Duration::from_millis(400));
    }
    drop(tab);
    drop(browser); // 关闭该 worker 的 Chrome 进程
}

/// 翻译并在失败时重试，保证质量（高效模式并发下偶发超时可自愈）。
fn translate_with_retry(tab: &Tab, cfg: &Config, img: &PathBuf, retries: u32) -> Result<Vec<u8>> {
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..=retries {
        match translate_one(tab, &cfg.source, &cfg.target, img, cfg.timeout) {
            Ok(b) => return Ok(b),
            Err(e) => {
                last = Some(e);
                if attempt < retries {
                    std::thread::sleep(Duration::from_millis(700));
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("未知错误")))
}

/// 探测本机 Google Chrome 可执行文件。优先用显式指定的路径，其次常见安装位置。
/// 返回 None 表示没找到（可据此在界面提示用户安装或手动指定）。
pub fn find_chrome(explicit: &Option<PathBuf>) -> Option<PathBuf> {
    // 1) 用户显式指定
    if let Some(p) = explicit {
        if p.is_file() {
            return Some(p.clone());
        }
    }
    // 2) 环境变量 CHROME
    if let Ok(p) = std::env::var("CHROME") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    // 3) 常见安装位置（系统级 / 用户级）
    let suffix = r"Google\Chrome\Application\chrome.exe";
    for env in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA", "ProgramW6432"] {
        if let Ok(base) = std::env::var(env) {
            let c = PathBuf::from(base).join(suffix);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

fn launch_browser(cfg: &Config) -> Result<Browser> {
    let extra_args: Vec<OsString> = vec![
        OsString::from("--disable-blink-features=AutomationControlled"),
        OsString::from("--no-first-run"),
        OsString::from("--no-default-browser-check"),
        OsString::from("--lang=zh-CN"),
    ];
    let arg_refs: Vec<&OsStr> = extra_args.iter().map(|s| s.as_os_str()).collect();

    let mut builder = LaunchOptions::default_builder();
    builder
        .headless(cfg.headless)
        .idle_browser_timeout(Duration::from_secs(3600))
        .window_size(Some((1280, 1000)))
        .args(arg_refs);
    // 用探测到的 Chrome 路径（比库自带探测更可靠）；探测不到则交给库兜底
    if let Some(p) = find_chrome(&cfg.chrome) {
        builder.path(Some(p));
    }
    let options = builder.build().map_err(|e| anyhow!("构造启动参数失败: {e}"))?;
    Browser::new(options)
        .context("无法启动 Chrome：请确认已安装 Google Chrome，或在“高级设置”里指定 chrome.exe 路径")
}

/// 翻译单张图片，返回翻译后图片字节。
pub fn translate_one(
    tab: &Tab,
    source: &str,
    target: &str,
    img: &PathBuf,
    timeout: Duration,
) -> Result<Vec<u8>> {
    let raw = fs::read(img).with_context(|| format!("读取图片失败: {}", img.display()))?;
    let b64 = STANDARD.encode(&raw);
    let mime = mime_of(img);

    let url = format!(
        "https://translate.google.com/?sl={}&tl={}&op=images",
        source, target
    );
    tab.navigate_to(&url).context("导航到图片翻译页失败")?;
    tab.wait_until_navigated().context("等待页面加载失败")?;

    wait_for_input(tab, Duration::from_secs(20)).context("未找到图片上传输入框")?;

    let upload_js = UPLOAD_TMPL.replace("__B64__", &b64).replace("__MIME__", mime);
    let r = eval_str(tab, &upload_js, true)?;
    if !r.starts_with("OK") {
        bail!("上传触发失败: {r}");
    }

    let deadline = Instant::now() + timeout;
    loop {
        let s = eval_str(tab, EXTRACT_JS, true)?;
        if let Some(b64out) = s.strip_prefix("B64:") {
            let bytes = STANDARD
                .decode(b64out.as_bytes())
                .context("base64 解码翻译图片失败")?;
            return Ok(bytes);
        }
        match s.as_str() {
            "PENDING" => {}
            "EMPTY" => bail!("响应中无图片（可能未识别到可翻译文字）"),
            other if other.starts_with("ERRX:") => bail!("解析响应出错: {other}"),
            other => bail!("未知响应: {other}"),
        }
        if Instant::now() >= deadline {
            bail!("超时未返回翻译结果");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn wait_for_input(tab: &Tab, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if eval_str(tab, INPUT_PRESENT_JS, false)? == "YES" {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("超时");
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn eval_str(tab: &Tab, expr: &str, await_promise: bool) -> Result<String> {
    let ro = tab.evaluate(expr, await_promise)?;
    Ok(match ro.value {
        Some(serde_json::Value::String(s)) => s,
        Some(other) => other.to_string(),
        None => String::new(),
    })
}

fn mime_of(p: &PathBuf) -> &'static str {
    match p
        .extension()
        .and_then(OsStr::to_str)
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    }
}
