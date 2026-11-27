//! avm CLI 入口。
//!
//! 子命令：
//!   avm doctor                 环境前置检测（WHPX 是否开启、与 HAXM 冲突等）
//!   avm start --toy            toy guest（真实 WHPX 上串口打印）
//!   avm daemon --listen <addr> 启动 avmd 守护进程（M6）
//!   avm list [--url <base>]    列出 daemon 实例
//!   avm create <id> [--vcpus N] [--mem MiB]
//!   avm boot|stop|delete <id>
//!
//! M6 起实例管理经 daemon HTTP API（默认 http://127.0.0.1:7878）。

use avm_client::Client;

const DEFAULT_BASE: &str = "http://127.0.0.1:7878";

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
            if args.iter().any(|a| a == "--toy") {
                // M8：平台探测 WHPX（Windows）→ KVM（Linux）→ Null
                let hv: Box<dyn avm_hypervisor::Hypervisor> = avm_hypervisor::detect();
                let hv = &*hv;
                println!("后端：{:?}", hv.backend());
                match avm_vmm::boot_toy_guest(hv) {
                    Ok(_) => println!("toy guest 已 halt"),
                    Err(e) => {
                        eprintln!("toy guest 启动失败：{e}");
                        std::process::exit(3);
                    }
                }
            } else {
                eprintln!("start：当前仅支持 --toy（Android 引导经 daemon/M5 真机阶段）");
                std::process::exit(2);
            }
        }
        "daemon" => {
            let listen = flag_value(&args, "--listen").unwrap_or_else(|| "127.0.0.1:7878".into());
            let addr: std::net::SocketAddr = match listen.parse() {
                Ok(a) => a,
                Err(e) => {
                    eprintln!("listen 地址非法 {listen}：{e}");
                    std::process::exit(2);
                }
            };
            println!("avmd 监听 {addr}");
            if let Err(e) = avm_daemon::run_daemon(addr) {
                eprintln!("avmd 退出：{e}");
                std::process::exit(3);
            }
        }
        "list" => {
            let c = Client::new(&base(&args));
            match c.list() {
                Ok(vms) => {
                    if vms.is_empty() {
                        println!("（无实例）");
                    }
                    for v in vms {
                        println!(
                            "{}\tvcpus={}\tmem={}MiB\t{}",
                            v.id, v.vcpus, v.memory_mib, v.state
                        );
                    }
                }
                Err(e) => die(&e),
            }
        }
        "create" => {
            let id = match args.get(2) {
                Some(i) => i.clone(),
                None => {
                    eprintln!("用法: avm create <id> [--vcpus N] [--mem MiB]");
                    std::process::exit(2);
                }
            };
            let vcpus = flag_value(&args, "--vcpus")
                .and_then(|v| v.parse().ok())
                .unwrap_or(2);
            let mem = flag_value(&args, "--mem")
                .and_then(|v| v.parse().ok())
                .unwrap_or(2048);
            let c = Client::new(&base(&args));
            match c.create(&id, vcpus, mem) {
                Ok(v) => println!("已创建 {} state={}", v.id, v.state),
                Err(e) => die(&e),
            }
        }
        "boot" | "stop" | "delete" => {
            let id = match args.get(2) {
                Some(i) => i.clone(),
                None => {
                    eprintln!("用法: avm {cmd} <id>");
                    std::process::exit(2);
                }
            };
            let c = Client::new(&base(&args));
            let r = match cmd {
                "boot" => c.boot(&id).map(|v| format!("{} → {}", v.id, v.state)),
                "stop" => c.stop(&id).map(|v| format!("{} → {}", v.id, v.state)),
                _ => c.delete(&id).map(|ok| {
                    if ok {
                        format!("{id} 已删除")
                    } else {
                        format!("{id} 不存在")
                    }
                }),
            };
            match r {
                Ok(msg) => println!("{msg}"),
                Err(e) => die(&e),
            }
        }
        _ => help(),
    }
}

fn base(args: &[String]) -> String {
    flag_value(args, "--url").unwrap_or_else(|| DEFAULT_BASE.into())
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn die(e: &avm_core::AvmError) {
    eprintln!("daemon 请求失败：{e}");
    std::process::exit(4);
}

fn help() {
    println!("avm —— Android x86_64 轻量运行时与设备底座");
    println!("用法: avm <doctor|start|daemon|list|create|boot|stop|delete>");
}
