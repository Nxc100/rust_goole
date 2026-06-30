//! 核心翻译逻辑：用 headless_chrome 驱动 Google 图片翻译。
//! GUI 与 CLI 共用。run_batch 通过回调上报进度，便于 GUI 实时刷新。

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use headless_chrome::{Browser, LaunchOptions, Tab};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const SUPPORTED_EXT: &[&str] = &["jpg", "jpeg", "png", "webp"];

/// 目标语言列表：(代码, 显示名)
pub const TARGET_LANGS: &[(&str, &str)] = &[
    ("en", "英语 English"),
    ("ja", "日语 日本語"),
    ("ko", "韩语 한국어"),
    ("zh-CN", "简体中文"),
    ("zh-TW", "繁体中文"),
    ("de", "德语 Deutsch"),
    ("fr", "法语 Français"),
    ("es", "西班牙语 Español"),
    ("ru", "俄语 Русский"),
    ("it", "意大利语 Italiano"),
    ("pt", "葡萄牙语 Português"),
    ("th", "泰语 ไทย"),
    ("vi", "越南语 Tiếng Việt"),
    ("ar", "阿拉伯语 العربية"),
    ("id", "印尼语 Indonesia"),
    ("hi", "印地语 हिन्दी"),
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

    on(Progress::Log(format!(
        "启动 Chrome（{}）...",
        if cfg.headless { "无头" } else { "可见窗口" }
    )));
    let browser = match launch_browser(cfg) {
        Ok(b) => b,
        Err(e) => {
            on(Progress::Fatal(format!("启动 Chrome 失败：{e}")));
            return;
        }
    };
    let tab = match browser.new_tab() {
        Ok(t) => t,
        Err(e) => {
            on(Progress::Fatal(format!("创建标签页失败：{e}")));
            return;
        }
    };

    let mut ok = 0usize;
    let mut failed = 0usize;

    for (i, img) in images.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            on(Progress::Log("已被用户停止。".into()));
            break;
        }
        // 相对路径（用于展示，并在输出目录复刻同样的子目录结构）
        let rel = img.strip_prefix(&cfg.input).unwrap_or(img.as_path());
        let name = rel.to_string_lossy().to_string();
        on(Progress::FileStart { idx: i, total, name: name.clone() });

        let out_path = cfg.output.join(rel);
        if out_path.exists() && !cfg.overwrite {
            ok += 1;
            on(Progress::FileOk { idx: i, name, path: out_path, bytes: 0, skipped: true });
            continue;
        }
        // 确保输出子目录存在
        if let Some(parent) = out_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                failed += 1;
                on(Progress::FileErr { idx: i, name, error: format!("创建输出子目录失败: {e}") });
                continue;
            }
        }

        match translate_one(&tab, &cfg.source, &cfg.target, img, cfg.timeout) {
            Ok(bytes) => match fs::write(&out_path, &bytes) {
                Ok(_) => {
                    ok += 1;
                    on(Progress::FileOk { idx: i, name, path: out_path, bytes: bytes.len(), skipped: false });
                }
                Err(e) => {
                    failed += 1;
                    on(Progress::FileErr { idx: i, name, error: format!("保存失败: {e}") });
                }
            },
            Err(e) => {
                failed += 1;
                on(Progress::FileErr { idx: i, name, error: format!("{e}") });
            }
        }

        std::thread::sleep(Duration::from_millis(800));
    }

    on(Progress::Done { ok, failed });
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
    if let Some(ref p) = cfg.chrome {
        builder.path(Some(p.clone()));
    }
    let options = builder.build().map_err(|e| anyhow!("构造启动参数失败: {e}"))?;
    Browser::new(options).context("请确认已安装 Chrome 或在高级设置里指定路径")
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
