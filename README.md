# MoInstaller

用 Rust 编写的 Windows 安装器工具链，目标是取代 Inno Setup 的核心使用场景：

> 开发者写一份 `installer.toml` 清单 → 运行 `mo build` → 得到单个 setup.exe
> （内嵌压缩文件 + 向导 GUI + 静默安装 + 卸载程序），双击即装，控制面板可卸载。

## 状态

按里程碑推进（设计文档见 `docs/plans/2026-10-06-moinstaller-rust-installer-spec.md`）：

- [ ] M1 骨架：workspace + mo-core（清单+overlay）+ mo-build + mo-setup 静默装文件
- [ ] M2 完整安装语义 + 事件总线 + L1 外部命令钩子
- [ ] egui 向导 + 主题 + 中英双语
- [ ] rhai 脚本扩展（L2）
- [ ] 打磨（自删、体积优化、文档、签名透传）

## 开发

```powershell
cargo build --workspace   # 注意：模板两阶段构建，首次需运行两次
cargo build --workspace
cargo test --workspace
```

mo-build 通过 build.rs 将 `target/release/mo-setup.exe` 嵌入为模板。开发期修改
mo-setup 后需重新 `cargo build --workspace` 两次使新模板生效。

## 协议

MIT OR Apache-2.0
