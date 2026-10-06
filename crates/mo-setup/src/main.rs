//! mo-setup：MoInstaller 安装器运行时（M1：静默安装文件）。

use mo_core::constants::ConstEnv;
use mo_core::overlay::{ATTR_READONLY, Package};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// 退出码语义（与计划一致）：
/// 0 成功；1 致命错误（包损坏等）；2 安装失败；3 磁盘不足；4 钩子/脚本错误。
const EXIT_OK: u8 = 0;
const EXIT_FATAL: u8 = 1;
const EXIT_INSTALL_FAILED: u8 = 2;

struct Args {
    dir: Option<String>,
    verysilent: bool,
    silent: bool,
}

fn parse_args(argv: &[String]) -> Args {
    let mut a = Args {
        dir: None,
        verysilent: false,
        silent: false,
    };
    for arg in argv {
        let lower = arg.to_ascii_lowercase();
        match lower.as_str() {
            "/verysilent" => a.verysilent = true,
            "/silent" => a.silent = true,
            _ => {
                if let Some(v) = arg
                    .strip_prefix("/DIR=")
                    .or_else(|| arg.strip_prefix("/dir="))
                {
                    a.dir = Some(v.trim_matches('"').to_string());
                }
                // 其余 Inno 风格参数（/GROUP= /NORESTART /SUPPRESSMSGBOXES 等）M1 暂忽略
            }
        }
    }
    a
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&argv);
    match run(&args) {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(RunError::Fatal(msg)) => {
            eprintln!("mo-setup 致命错误: {msg}");
            ExitCode::from(EXIT_FATAL)
        }
        Err(RunError::InstallFailed(msg)) => {
            eprintln!("mo-setup 安装失败: {msg}");
            ExitCode::from(EXIT_INSTALL_FAILED)
        }
    }
}

enum RunError {
    Fatal(String),
    InstallFailed(String),
}

impl From<mo_core::Error> for RunError {
    fn from(e: mo_core::Error) -> Self {
        match e {
            mo_core::Error::Io(io) => RunError::InstallFailed(io.to_string()),
            other => RunError::Fatal(other.to_string()),
        }
    }
}

impl From<std::io::Error> for RunError {
    fn from(e: std::io::Error) -> Self {
        RunError::InstallFailed(e.to_string())
    }
}

fn run(args: &Args) -> Result<(), RunError> {
    let exe: PathBuf = std::env::current_exe().map_err(|e| RunError::Fatal(e.to_string()))?;
    let mut pkg = Package::open(&exe).map_err(|e| RunError::Fatal(e.to_string()))?;
    let manifest = pkg.manifest.clone();

    // M1 无 GUI：任何模式都按静默安装处理（M3 引入 egui 向导）
    if !args.silent && !args.verysilent {
        println!(
            "MoInstaller M1 骨架：{} {}（GUI 将于 M3 提供，本次以静默模式安装）",
            manifest.app.name, manifest.app.version
        );
    }

    // 目标目录：/DIR= > default_dir 展开（default_dir 不含 {app}，可先展开）
    let base_env = ConstEnv::from_process_env();
    let target_str = match &args.dir {
        Some(d) => d.clone(),
        None => base_env
            .expand(&manifest.options.default_dir)
            .map_err(|e| RunError::Fatal(format!("展开 default_dir 失败: {e}")))?,
    };
    let target = PathBuf::from(&target_str);
    let env = base_env.with_app(&target, &manifest.app.name);

    println!("安装到: {}", target.display());
    let mut installed: usize = 0;
    let mut total_bytes: u64 = 0;

    for (i, rule) in manifest.files.iter().enumerate() {
        let dst_root = env
            .expand(&rule.dst)
            .map_err(|e| RunError::Fatal(format!("展开 files.dst 失败: {e}")))?;
        let prefix = format!("f{i}/");
        for entry in pkg.entries_with_prefix(&prefix) {
            let rel = &entry.path[prefix.len()..];
            let dst = Path::new(&dst_root).join(rel);
            let data = pkg.read_entry(&entry)?;

            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            // 覆盖已存在的只读文件前先清只读位
            if dst.exists() {
                let _ = clear_readonly(&dst);
            }
            std::fs::write(&dst, &data)?;
            if entry.attrs & ATTR_READONLY != 0 {
                set_readonly(&dst);
            }
            installed += 1;
            total_bytes += data.len() as u64;
            println!("  + {}", entry.path);
        }
    }

    println!(
        "完成：{installed} 个文件，{total_bytes} 字节（{}）",
        human_bytes(total_bytes)
    );
    Ok(())
}

// Windows 专用安装器：覆盖只读文件前清只读位（Unix world-writable 提示不适用）
#[allow(clippy::permissions_set_readonly_false)]
fn clear_readonly(p: &Path) -> std::io::Result<()> {
    let mut perm = std::fs::metadata(p)?.permissions();
    perm.set_readonly(false);
    std::fs::set_permissions(p, perm)
}

fn set_readonly(p: &Path) {
    if let Ok(md) = std::fs::metadata(p) {
        let mut perm = md.permissions();
        perm.set_readonly(true);
        let _ = std::fs::set_permissions(p, perm);
    }
}

fn human_bytes(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}
