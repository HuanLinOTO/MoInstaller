//! mo-setup：MoInstaller 安装器运行时。
//!
//! 双模式：默认安装（GUI 于 M3 提供，当前按静默处理）；
//! --uninstall 为卸载模式（自身即 {app}/mo-uninstall.exe）。
//! 退出码：0 成功；1 致命；2 安装失败（已回滚）；3 磁盘不足；4 钩子错误。

use mo_core::constants::ConstEnv;
use mo_core::manifest::Manifest;
use mo_core::overlay::Package;
use mo_engine::event::{EngineCtx, EventBus};
use mo_engine::executor::{EngineError, Executor};
use mo_engine::hook::HookRunner;
use mo_engine::win::misc;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::ExitCode;

struct Args {
    dir: Option<String>,
    group: Option<String>,
    verysilent: bool,
    silent: bool,
    uninstall: bool,
    raw: Vec<String>,
}

fn parse_args(argv: &[String]) -> Args {
    let mut a = Args {
        dir: None,
        group: None,
        verysilent: false,
        silent: false,
        uninstall: false,
        raw: argv.to_vec(),
    };
    for arg in argv {
        match arg.to_ascii_lowercase().as_str() {
            "/verysilent" => a.verysilent = true,
            "/silent" => a.silent = true,
            "/uninstall" | "--uninstall" => a.uninstall = true,
            _ => {
                if let Some(v) = arg
                    .strip_prefix("/DIR=")
                    .or_else(|| arg.strip_prefix("/dir="))
                {
                    a.dir = Some(v.trim_matches('"').to_string());
                } else if let Some(v) = arg
                    .strip_prefix("/GROUP=")
                    .or_else(|| arg.strip_prefix("/group="))
                {
                    a.group = Some(v.trim_matches('"').to_string());
                }
                // 其余 Inno 风格参数（/NORESTART /SUPPRESSMSGBOXES 等）忽略
            }
        }
    }
    a
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = parse_args(&argv);
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("mo-setup: {}", e.message());
            ExitCode::from(e.exit_code())
        }
    }
}

fn fatal(msg: impl Into<String>) -> EngineError {
    EngineError::Fatal(msg.into())
}

fn run(args: &Args) -> Result<(), EngineError> {
    let exe: PathBuf = std::env::current_exe().map_err(|e| fatal(format!("定位自身失败: {e}")))?;
    let mut pkg = Package::open(&exe).map_err(|e| fatal(format!("解析安装包: {e}")))?;
    let manifest = pkg.manifest.clone();
    // M1/M2 无 GUI：任何模式都按静默处理（M3 引入 egui 向导）
    let silent = true;

    // 卸载模式：显式参数，或自身文件名即 mo-uninstall.exe（复制自安装器）
    let is_uninstaller = exe
        .file_stem()
        .map(|s| s.eq_ignore_ascii_case("mo-uninstall"))
        .unwrap_or(false);
    if args.uninstall || is_uninstaller {
        return uninstall_flow(&manifest, &exe, silent);
    }
    install_flow(&manifest, &mut pkg, &exe, args, silent)
}

fn install_flow(
    manifest: &Manifest,
    pkg: &mut Package,
    exe: &std::path::Path,
    args: &Args,
    silent: bool,
) -> Result<(), EngineError> {
    // 提权检查
    if manifest.options.require_admin && !misc::is_elevated() {
        misc::relaunch_elevated(exe, &args.raw)
            .map_err(|e| fatal(format!("需要管理员权限，重启失败: {e}")))?;
        return Ok(()); // 新进程接手
    }

    // 单实例互斥体
    let _mutex = misc::SingleInstance::new(&format!("MoInstaller.{}", manifest.app.id))
        .ok_or_else(|| fatal("安装程序已在运行"))?;

    // 目标目录
    let base_env = ConstEnv::from_process_env();
    let target_str = match &args.dir {
        Some(d) => d.clone(),
        None => base_env
            .expand(&manifest.options.default_dir)
            .map_err(|e| fatal(format!("展开 default_dir: {e}")))?,
    };
    let target = PathBuf::from(&target_str);
    let group_name = args
        .group
        .clone()
        .unwrap_or_else(|| manifest.app.name.clone());
    let env = base_env.with_app(&target, &group_name);

    println!(
        "安装 {} {} 到: {}",
        manifest.app.name,
        manifest.app.version,
        target.display()
    );

    let ctx = EngineCtx {
        app_dir: target.clone(),
        app_name: manifest.app.name.clone(),
        app_id: manifest.app.id.clone(),
        version: manifest.app.version.clone(),
        silent,
        selected_components: manifest
            .components
            .iter()
            .map(|c| c.id.clone())
            .collect::<BTreeSet<_>>(),
        env,
    };
    let mut bus = EventBus::new();
    if !manifest.hooks.is_empty() {
        bus.subscribe(Box::new(HookRunner::new(manifest.hooks.clone())));
    }
    let mut executor = Executor::new(bus, ctx, target.join("mo-install.log"));
    executor.install(manifest, pkg, exe)
}

fn uninstall_flow(
    manifest: &Manifest,
    exe: &std::path::Path,
    silent: bool,
) -> Result<(), EngineError> {
    let app_dir = exe
        .parent()
        .ok_or_else(|| fatal("无法定位安装目录"))?
        .to_path_buf();
    let env = ConstEnv::from_process_env().with_app(&app_dir, &manifest.app.name);

    println!("卸载 {} ({})", manifest.app.name, app_dir.display());

    let ctx = EngineCtx {
        app_dir,
        app_name: manifest.app.name.clone(),
        app_id: manifest.app.id.clone(),
        version: manifest.app.version.clone(),
        silent,
        selected_components: BTreeSet::new(),
        env,
    };
    let mut bus = EventBus::new();
    if !manifest.hooks.is_empty() {
        bus.subscribe(Box::new(HookRunner::new(manifest.hooks.clone())));
    }
    let log_path = ctx.app_dir.join("mo-install.log");
    let mut executor = Executor::new(bus, ctx, log_path);
    executor.uninstall(manifest, exe)
}
