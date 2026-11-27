//! 日志系统（分级、按天分文件、保留天数自动清理、并发写入控制）。
//!
//! 设计（对应 PLAN 需求四点）：
//! 1. 分级配置：`Level` 五级过滤，`AVM_LOG_LEVEL` 环境变量可配
//! 2. 保存天数：`retention_days`，`AVM_LOG_RETENTION_DAYS` 可配
//! 3. 过期清理：init 时 + 每跨天一次，按文件名日期（avm-YYYYMMDD.log）删除过期
//! 4. 并发控制：全局单写线程 + mpsc 通道——调用方只做非阻塞 send，
//!    所有磁盘写串行在一个后台线程，锁竞争只发生在 sender clone（OnceLock 读取）
//!
//! 未 init 时所有日志调用为静默 no-op（库代码安全）。

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// 日志级别（数值越小越详尽）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        }
    }

    /// 解析 "debug"/"INFO" 等大小写不敏感字面量。
    pub fn parse(s: &str) -> Option<Level> {
        match s.trim().to_ascii_lowercase().as_str() {
            "trace" => Some(Level::Trace),
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" => Some(Level::Error),
            _ => None,
        }
    }
}

/// 日志配置。
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// 日志目录（默认 ./logs）
    pub dir: PathBuf,
    /// 最低输出级别
    pub level: Level,
    /// 日志保存天数（超过自动删除）
    pub retention_days: u32,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("logs"),
            level: Level::Info,
            retention_days: 7,
        }
    }
}

impl LogConfig {
    /// 环境变量覆盖：AVM_LOG_DIR / AVM_LOG_LEVEL / AVM_LOG_RETENTION_DAYS。
    pub fn from_env() -> LogConfig {
        let mut c = LogConfig::default();
        if let Ok(d) = std::env::var("AVM_LOG_DIR") {
            if !d.trim().is_empty() {
                c.dir = PathBuf::from(d);
            }
        }
        if let Ok(l) = std::env::var("AVM_LOG_LEVEL") {
            if let Some(lv) = Level::parse(&l) {
                c.level = lv;
            }
        }
        if let Ok(n) = std::env::var("AVM_LOG_RETENTION_DAYS") {
            if let Ok(n) = n.trim().parse::<u32>() {
                if n > 0 {
                    c.retention_days = n;
                }
            }
        }
        c
    }
}

/// 文件名日期：`avm-YYYYMMDD.log`。
fn file_name_for(day: u64) -> String {
    format!("avm-{day:08}.log")
}

/// 从文件名解析 day 序号；非日志文件返回 None。
fn parse_file_day(name: &str) -> Option<u64> {
    let rest = name.strip_prefix("avm-")?.strip_suffix(".log")?;
    if rest.len() != 8 || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    rest.parse::<u64>().ok()
}

/// 当前「天序号」（自 Unix epoch 的天数；闰年由算法自然处理）。
pub fn today_day() -> u64 {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs / 86_400
}

/// 天序号 → YYYYMMDD（civil-from-days，Howard Hinnant 算法）。
pub fn day_to_ymd(day: u64) -> (u32, u32, u32) {
    let z = day as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as u32, m as u32, d as u32)
}

struct LogMsg {
    dir: PathBuf,
    day: u64,
    line: String,
}

/// 后台写线程主循环：串行写盘 + 跨天轮换 + 每跨天清理过期。
fn writer_loop(rx: std::sync::mpsc::Receiver<LogMsg>, retention_days: u32) {
    let mut cur_day: u64 = 0;
    let mut cur_dir: Option<PathBuf> = None;
    let mut cur_path: Option<PathBuf> = None;
    let mut out: Option<std::fs::File> = None;
    while let Ok(msg) = rx.recv() {
        if msg.day != cur_day || cur_dir.as_ref() != Some(&msg.dir) {
            cur_day = msg.day;
            cur_dir = Some(msg.dir.clone());
            let dir = &msg.dir;
            let _ = std::fs::create_dir_all(dir);
            out = None;
            cleanup_expired(dir, cur_day, retention_days);
        }
        // 文件被外部删除/轮转时自动重开（append 语义下写已 unlink 的句柄会静默丢失）
        let path = msg.dir.join(file_name_for(cur_day));
        if cur_path.as_ref() != Some(&path) || !path.exists() {
            cur_path = Some(path.clone());
            let _ = std::fs::create_dir_all(&msg.dir);
            out = match std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                Ok(f) => {
                    STAT_OPEN_OK.fetch_add(1, Ordering::Relaxed);
                    Some(f)
                }
                Err(e) => {
                    eprintln!("avm-log: 日志文件打开失败 {e}（本条丢弃）");
                    None
                }
            };
        }
        if let Some(f) = out.as_mut() {
            use std::io::Write;
            let _ = f.write_all(msg.line.as_bytes());
            // 行级 flush：保证崩溃/停机时日志完整（日志量级低，可接受）
            let _ = f.flush();
            STAT_WRITTEN.fetch_add(1, Ordering::Relaxed);
            // 涌入丢弃摘要：队列恢复后立即补记
            let dropped = DROPPED.swap(0, Ordering::Relaxed);
            if dropped > 0 {
                let _ = write!(f, "… [WARN] 日志队列涌满，丢弃 {dropped} 条低优先日志\n");
                let _ = f.flush();
                STAT_WRITTEN.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// 超长正文截断（按字节、不切 UTF-8 字符）。
pub fn truncate_body(msg: &str) -> String {
    if msg.len() <= MAX_LINE_BYTES {
        return msg.to_string();
    }
    let mut cut = MAX_LINE_BYTES;
    while cut > 0 && !msg.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…(超长截断)", &msg[..cut])
}

/// 删除 day < today - retention 的日志文件。
pub fn cleanup_expired(dir: &PathBuf, today: u64, retention_days: u32) -> usize {
    let cutoff = today.saturating_sub(retention_days as u64);
    let mut removed = 0usize;
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.filter_map(|e| e.ok()) {
            let name = e.file_name();
            let name = match name.to_str() {
                Some(n) => n,
                None => continue,
            };
            if let Some(day) = parse_file_day(name) {
                if day < cutoff {
                    if std::fs::remove_file(e.path()).is_ok() {
                        removed += 1;
                    }
                }
            }
        }
    }
    removed
}

/// 通道容量：涌入超过此数的日志将被丢弃（背压保护，绝不阻塞调用方）
const QUEUE_CAP: usize = 8192;
/// 单行正文最大字节数（超长截断）
pub const MAX_LINE_BYTES: usize = 8 * 1024;

static GLOBAL: OnceLock<SyncSender<LogMsg>> = OnceLock::new();
static MIN_LEVEL: OnceLock<Level> = OnceLock::new();
static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();
/// 因队列满被丢弃的日志条数（写线程恢复后补记摘要）
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// 历史累计丢弃（永不重置；测试用精确记账）
static TOTAL_DROPPED: AtomicU64 = AtomicU64::new(0);
/// 运行统计（测试与排障观察写线程行为）
static STAT_OPEN_OK: AtomicU64 = AtomicU64::new(0);
static STAT_OPEN_ERR: AtomicU64 = AtomicU64::new(0);
static STAT_WRITTEN: AtomicU64 = AtomicU64::new(0);

/// 写线程运行统计：(打开成功, 打开失败, 已写行数)。
pub fn stats() -> (u64, u64, u64) {
    (
        STAT_OPEN_OK.load(Ordering::Relaxed),
        STAT_OPEN_ERR.load(Ordering::Relaxed),
        STAT_WRITTEN.load(Ordering::Relaxed),
    )
}

/// 初始化全局日志（幂等：重复 init 只保留第一次）。失败静默（日志不应致命）。
pub fn init(cfg: LogConfig) {
    if GLOBAL.get().is_some() {
        return;
    }
    let (tx, rx) = sync_channel::<LogMsg>(QUEUE_CAP);
    let retention = cfg.retention_days;
    let _ = std::thread::Builder::new()
        .name("avm-log-writer".into())
        .spawn(move || writer_loop(rx, retention));
    let _ = GLOBAL.set(tx);
    let _ = MIN_LEVEL.set(cfg.level);
    let _ = LOG_DIR.set(cfg.dir.clone());
}

/// 便捷：按环境变量配置初始化。
pub fn init_from_env() {
    init(LogConfig::from_env());
}

/// 写一条日志（级别过滤 + 非阻塞投递）。
pub fn log(level: Level, msg: &str) {
    let Some(tx) = GLOBAL.get() else { return };
    if let Some(min) = MIN_LEVEL.get() {
        if level < *min {
            return;
        }
    }
    let day = today_day();
    let (y, m, d) = day_to_ymd(day);
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|t| t.as_secs() % 86_400)
        .unwrap_or(0);
    let (hh, mm, ss) = (secs / 3600, secs % 3600 / 60, secs % 60);
    let body = truncate_body(msg);
    let mut line = String::with_capacity(body.len() + 48);
    let _ = write!(
        &mut line,
        "{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02} [{}] {body}\n",
        level.as_str()
    );
    let dir = LOG_DIR
        .get()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("logs"));
    // 非阻塞投递：队列满即丢弃并计数（绝不阻塞 VM 线程）
    if tx.try_send(LogMsg { dir, day, line }).is_err() {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        TOTAL_DROPPED.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn trace(msg: &str) {
    log(Level::Trace, msg)
}
pub fn debug(msg: &str) {
    log(Level::Debug, msg)
}
pub fn info(msg: &str) {
    log(Level::Info, msg)
}
pub fn warn(msg: &str) {
    log(Level::Warn, msg)
}
pub fn error(msg: &str) {
    log(Level::Error, msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn test_config(dir: PathBuf, level: Level) -> LogConfig {
        LogConfig {
            dir,
            level,
            retention_days: 3,
        }
    }

    fn read_log(dir: &PathBuf, day: u64) -> String {
        std::fs::read_to_string(dir.join(file_name_for(day))).unwrap_or_default()
    }

    fn wait_until(dir: &PathBuf, day: u64, want: usize) {
        for _ in 0..200 {
            if read_log(dir, day).lines().count() >= want {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[test]
    fn level_parse_and_order() {
        assert_eq!(Level::parse("warn"), Some(Level::Warn));
        assert_eq!(Level::parse("ERROR"), Some(Level::Error));
        assert_eq!(Level::parse("nope"), None);
        assert!(Level::Debug < Level::Info);
    }

    #[test]
    fn day_to_ymd_known_dates() {
        // 2026-11-27 的 epoch 天数：20784
        assert_eq!(day_to_ymd(20_784), (2026, 11, 27));
        // epoch 起点当天
        assert_eq!(day_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn global_logger_e2e_filter_and_concurrency() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = test_dir();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        init(test_config(dir.clone(), Level::Info));
        debug("should be filtered");
        info("hello info");
        warn("hello warn");
        // 8 线程 × 50 条并发
        let threads: Vec<_> = (0..8)
            .map(|t| {
                std::thread::spawn(move || {
                    for i in 0..50 {
                        info(&format!("t{t}-m{i}"));
                    }
                })
            })
            .collect();
        for h in threads {
            let _ = h.join();
        }
        wait_until(&dir, today_day(), 2 + 400);
        let content = read_log(&dir, today_day());
        let got = content.lines().count();
        assert!(
            got >= 402,
            "3 条直写 + 8 线程 × 50 条应全部落盘（实际 {got}，其他测试线程的日志合法共盘）"
        );
        assert!(content.contains("hello info"));
        assert!(content.contains("hello warn"));
        assert!(!content.contains("should be filtered"));
        // 无交错损坏：每行都以完整时间戳开头
        assert!(content.lines().all(|l| l.starts_with("20")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleanup_removes_expired_only() {
        let dir = std::env::temp_dir().join(format!("avm-log-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let today = today_day();
        let cases: Vec<(String, bool)> = vec![
            (file_name_for(today - 10), false), // 过期 → 删
            (file_name_for(today - 1), true),   // 在保留期 → 留
            (file_name_for(today), true),
            ("not-a-log.txt".into(), true), // 非日志文件不动
            ("avm-bad.log".into(), true),   // 文件名无法解析日期 → 不动
        ];
        for (name, _) in &cases {
            let _ = std::fs::write(dir.join(name), b"x");
        }
        let removed = cleanup_expired(&dir, today, 3);
        assert_eq!(removed, 1);
        let remaining: Vec<String> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        assert!(remaining.contains(&file_name_for(today - 1)));
        assert!(remaining.contains(&"not-a-log.txt".to_string()));
        assert!(!remaining.contains(&file_name_for(today - 10)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn truncate_body_respects_limit_and_utf8() {
        let short = "hello";
        assert_eq!(truncate_body(short), short);
        let long = "喵".repeat(10_000); // 30000 字节
        let t = truncate_body(&long);
        assert!(t.len() <= MAX_LINE_BYTES + "…(超长截断)".len());
        assert!(t.ends_with("…(超长截断)"));
        assert!(t.starts_with("喵"));
    }

    #[test]
    fn surge_writes_never_block_and_still_ordered() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = test_dir();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        init(test_config(dir.clone(), Level::Info));
        let dropped_before = TOTAL_DROPPED.load(Ordering::Relaxed);
        // 4 线程 × 5000 条远超 QUEUE_CAP：绝不阻塞、绝不 panic
        let threads: Vec<_> = (0..4)
            .map(|t| {
                std::thread::spawn(move || (0..5000).for_each(|i| info(&format!("s{t}-i{i}"))))
            })
            .collect();
        for h in threads {
            let _ = h.join();
        }
        // 记账守恒：每条消息要么落盘、要么被 try_send 丢弃并计数；轮询直到队列排空
        let mut landed = 0usize;
        for _ in 0..1000 {
            let content = read_log(&dir, today_day());
            landed = content.lines().filter(|l| l.contains("-i")).count();
            let dropped = (TOTAL_DROPPED.load(Ordering::Relaxed) - dropped_before) as usize;
            if landed + dropped >= 20_000 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let dropped = (TOTAL_DROPPED.load(Ordering::Relaxed) - dropped_before) as usize;

        assert_eq!(
            landed + dropped,
            20_000,
            "涌入守恒：落盘 {landed} + 丢弃 {dropped} = 20000"
        );
        assert!(landed > 0, "丢弃可以有，但日志系统不能全丢");
        let content = read_log(&dir, today_day());
        assert!(content
            .lines()
            .all(|l| l.starts_with("20") || l.contains("丢弃")));
    }

    /// 全局单例测试必须串行（同一进程共享 writer 线程与 LOG_DIR）。
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn test_dir() -> PathBuf {
        std::env::temp_dir().join(format!("avm-log-tests-{}", std::process::id()))
    }

    #[test]
    fn noop_before_init() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // 未 init 时静默：不 panic、不写文件
        // （init 已被其他测试调用过；这里仅保证调用安全）
        let hits = Arc::new(AtomicUsize::new(0));
        let h2 = Arc::clone(&hits);
        let _ = std::thread::spawn(move || {
            info("safe even if racing init");
            h2.fetch_add(1, Ordering::SeqCst);
        })
        .join();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}
