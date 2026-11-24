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
        "doctor" => match avm_hypervisor::doctor::report() {
            Ok(true) => println!("环境检测通过"),
            Ok(false) => {
                println!("环境检测未通过");
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("doctor 失败：{e}");
                std::process::exit(2);
            }
        },
        "start" => {
            // M0：--toy 走 toy guest（真实 WHPX 上串口打印；非 Windows 报错）
            if args.iter().any(|a| a == "--toy") {
                // Windows 上用真实 WHPX；其它平台回退 Null（报错提示）
                let hv: Box<dyn avm_hypervisor::Hypervisor> =
                    match avm_hypervisor::whp::WhpHypervisor::new() {
                        Ok(whp) => Box::new(whp),
                        Err(_) => Box::new(avm_hypervisor::NullHypervisor),
                    };
                let hv = &*hv;
                match avm_vmm::boot_toy_guest(hv) {
                    Ok(_) => println!("toy guest 已 halt"),
                    Err(e) => {
                        eprintln!("toy guest 启动失败：{e}");
                        std::process::exit(3);
                    }
                }
            } else {
                eprintln!("start：等待 M1 Linux 引导（当前仅支持 --toy）");
                std::process::exit(2);
            }
        }
        "list" => list(),
        "stop" => {
            eprintln!("stop：等待 M6 daemon/API 实现");
            std::process::exit(2);
        }
        _ => help(),
    }
}

fn list() {
    println!("avm list —— M6 后连接 avmd 查询实例");
}

fn help() {
    println!("avm —— Android x86_64 轻量运行时与设备底座");
    println!("用法: avm <doctor|list|start|stop>");
}
