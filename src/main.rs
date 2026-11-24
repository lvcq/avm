//! avm CLI 入口。
//!
//! 子命令：
//!   avm doctor    环境前置检测（WHPX 是否开启、与 HAXM 冲突等）
//!   avm start     启动一台实例
//!   avm stop      停止实例
//!   avm list      列出实例
//!   avmd          后台守护进程（见 avm-daemon）
//!
//! M6 前：本地直连 VMM；M6 后：作为 daemon 客户端。

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("help");
    match cmd {
        "doctor" => doctor(),
        "list" => list(),
        "start" | "stop" => {
            eprintln!("{cmd}：等待 M6 daemon/API 实现");
            std::process::exit(2);
        }
        _ => help(),
    }
}

fn doctor() {
    println!("avm doctor —— 环境检测（M0 里程碑填充真实检测项）");
    println!("  [ ] WHPX 已启用（Windows 可选功能：虚拟机平台）");
    println!("  [ ] 与 HAXM 无冲突");
    println!("  [ ] WinHvPlatform.dll 可加载");
}

fn list() {
    println!("avm list —— M6 后连接 avmd 查询实例");
}

fn help() {
    println!("avm —— Android x86_64 轻量运行时与设备底座");
    println!("用法: avm <doctor|list|start|stop>");
}
