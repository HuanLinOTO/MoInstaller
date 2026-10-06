//! M1 端到端：build → /VERYSILENT 安装 → 断言文件树。
//!
//! 模板直接取本 crate 刚构建的 mo-setup.exe（CARGO_BIN_EXE_mo-setup），
//! 不依赖两阶段模板固化，可与 cargo test --workspace 一起跑。

use std::fs;
use std::path::Path;
use std::process::Command;

const APP_EXE_BYTES: &[u8] = b"#!/usr/bin/env fake-exe\x00\x01\x02 moinstaller m1 e2e payload";
const README_ZH: &str = "MoInstaller M1 端到端测试\n中文内容 with spaces & 特殊字符 !@#$%\n";

fn setup_bin() -> &'static str {
    env!("CARGO_BIN_EXE_mo-setup")
}

fn make_fixture(tmp: &Path) -> std::path::PathBuf {
    let dist = tmp.join("dist");
    fs::create_dir_all(dist.join("bin")).unwrap();
    fs::create_dir_all(dist.join("数据 目录")).unwrap();
    fs::write(dist.join("bin").join("app.exe"), APP_EXE_BYTES).unwrap();
    fs::write(dist.join("readme-zh.txt"), README_ZH.as_bytes()).unwrap();
    fs::write(
        dist.join("数据 目录").join("配置 文件.json"),
        br#"{"k":"v"}"#,
    )
    .unwrap();
    fs::write(dist.join("empty.dat"), b"").unwrap();

    let toml = tmp.join("installer.toml");
    fs::write(
        &toml,
        r#"
[app]
id = "com.example.e2e"
name = "E2E Demo 应用"
version = "0.1.0"
publisher = "MoInstaller Tests"

[options]
default_dir = '{localappdata}\E2E Demo 应用'

[[files]]
src = "dist/**/*"
dst = "{app}"
"#,
    )
    .unwrap();
    toml
}

fn run_setup(setup: &Path, dir: &Path) -> std::process::Output {
    Command::new(setup)
        .arg("/VERYSILENT")
        .arg(format!("/DIR={}", dir.display()))
        .output()
        .expect("运行 setup.exe 失败")
}

#[test]
fn e2e_silent_install_files() {
    let tmp = tempfile::tempdir().unwrap();
    let toml = make_fixture(tmp.path());
    let out_exe = tmp.path().join("demo-setup.exe");

    // build
    let stats = mo_build::build(
        &toml,
        &mo_build::BuildOptions {
            template: Some(setup_bin().into()),
            out: Some(out_exe.clone()),
        },
    )
    .unwrap_or_else(|e| panic!("build 失败: {e}"));
    assert_eq!(stats.file_count, 4);
    assert!(out_exe.is_file());

    // 静默安装（目标目录带中文与空格）
    let inst_dir = tmp.path().join("安装 目标");
    let out = run_setup(&out_exe, &inst_dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "安装失败 exit={:?}\nstdout:\n{stdout}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );

    // 断言文件树与内容
    assert_eq!(
        fs::read(inst_dir.join("bin").join("app.exe")).unwrap(),
        APP_EXE_BYTES
    );
    assert_eq!(
        fs::read_to_string(inst_dir.join("readme-zh.txt")).unwrap(),
        README_ZH
    );
    assert_eq!(
        fs::read(inst_dir.join("数据 目录").join("配置 文件.json")).unwrap(),
        br#"{"k":"v"}"#
    );
    assert_eq!(fs::read(inst_dir.join("empty.dat")).unwrap(), b"");
}

#[test]
fn e2e_bare_template_without_overlay_fails() {
    // 无 overlay 的裸 mo-setup.exe 应以非 0 退出
    let out = Command::new(setup_bin())
        .arg("/VERYSILENT")
        .output()
        .unwrap();
    assert_ne!(out.status.code(), Some(0));
    assert!(!out.stderr.is_empty());
}

#[test]
fn e2e_rerun_overwrites() {
    // 重复安装 = 覆盖式，仍然成功
    let tmp = tempfile::tempdir().unwrap();
    let toml = make_fixture(tmp.path());
    let out_exe = tmp.path().join("demo-setup.exe");
    mo_build::build(
        &toml,
        &mo_build::BuildOptions {
            template: Some(setup_bin().into()),
            out: Some(out_exe.clone()),
        },
    )
    .unwrap();
    let inst_dir = tmp.path().join("twice");
    for _ in 0..2 {
        let out = run_setup(&out_exe, &inst_dir);
        assert!(out.status.success(), "二次安装应成功");
    }
    assert!(inst_dir.join("bin").join("app.exe").is_file());
}
