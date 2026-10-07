//! egui 安装向导：欢迎 → 许可 → 目录 → 组件 → 安装 → 完成。
//!
//! 布局：左侧品牌栏（accent 压暗底或应用侧图）+ 中央内容区 + 底部按钮条。
//! 引擎跑在工作线程，经 channel 推送进度；GUI 是事件总线的另一个订阅者视角。

use crate::picker;
use crate::strings::Lang;
use crate::theme::{Page, ThemeRuntime};
use egui::{Color32, Layout, RichText, Sense};
use mo_core::constants::ConstEnv;
use mo_core::manifest::Manifest;
use mo_core::overlay::Package;
use mo_engine::event::{Decision, EngineCtx, Event, EventBus, Subscriber};
use mo_engine::executor::{EngineError, Executor};
use mo_engine::hook::HookRunner;
use mo_engine::win::misc;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

const TEXT_SOFT: Color32 = Color32::from_rgb(0x4A, 0x51, 0x5A);

/// 半透明白（侧栏副文案）。
fn white_a(a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, (a * 255.0) as u8)
}

/// 安装线程 → UI 的消息。
pub enum UiMsg {
    Step(String),
    File {
        path: String,
        done: usize,
        total: usize,
    },
    Done(Result<(), EngineError>),
}

pub struct WizardApp {
    pub manifest: Manifest,
    pub self_exe: PathBuf,
    pub theme: ThemeRuntime,
    pub banner_bytes: Option<Vec<u8>>,
    pub sidebar_bytes: Option<Vec<u8>>,
    pub dir: String,
    pub components: BTreeMap<String, bool>,
    pub license_text: String,
    pub license_accepted: bool,
    pub page: Page,
    pub install_rx: Option<Receiver<UiMsg>>,
    pub progress: Progress,
    pub result: Option<Result<(), EngineError>>,
    pub run_after: bool,
    pub closed: bool,
    /// 卸载模式
    pub uninstall_mode: bool,
    pub uninstall_keep: bool,
    /// 当前页进入时刻（页面切换入场动画用）。
    pub page_enter: std::time::Instant,
}

#[derive(Default, Clone)]
pub struct Progress {
    pub step: String,
    pub current_file: String,
    pub files_done: usize,
    pub files_total: usize,
    pub finished: bool,
}

impl WizardApp {
    /// GUI 安装向导。
    #[allow(clippy::too_many_arguments)]
    pub fn install_wizard(
        manifest: Manifest,
        self_exe: PathBuf,
        default_dir: String,
        banner: Option<Vec<u8>>,
        sidebar: Option<Vec<u8>>,
        license_text: String,
    ) -> Self {
        let theme = ThemeRuntime::new(
            &manifest,
            &egui::Context::default(),
            Lang::detect(),
            None,
            None,
        );
        let components = manifest
            .components
            .iter()
            .map(|c| (c.id.clone(), c.default_selected()))
            .collect();
        Self {
            page: theme.pages.first().copied().unwrap_or(Page::Install),
            theme,
            manifest,
            self_exe,
            banner_bytes: banner,
            sidebar_bytes: sidebar,
            dir: default_dir,
            components,
            license_text,
            license_accepted: false,
            install_rx: None,
            progress: Progress::default(),
            result: None,
            run_after: false,
            closed: false,
            uninstall_mode: false,
            uninstall_keep: true,
            page_enter: std::time::Instant::now(),
        }
    }

    /// GUI 卸载确认。
    pub fn uninstaller(manifest: Manifest, self_exe: PathBuf) -> Self {
        let theme = ThemeRuntime::new(
            &manifest,
            &egui::Context::default(),
            Lang::detect(),
            None,
            None,
        );
        Self {
            page: Page::Finish,
            theme,
            manifest,
            self_exe,
            banner_bytes: None,
            sidebar_bytes: None,
            dir: String::new(),
            components: BTreeMap::new(),
            license_text: String::new(),
            license_accepted: false,
            install_rx: None,
            progress: Progress::default(),
            result: None,
            run_after: false,
            closed: false,
            uninstall_mode: true,
            uninstall_keep: true,
            page_enter: std::time::Instant::now(),
        }
    }

    /// 安装/卸载工作线程运行中（取消与窗口关闭必须被拦截）。
    fn running(&self) -> bool {
        self.install_rx.is_some() && !self.progress.finished
    }

    /// 切页并重置入场动画时钟。
    fn set_page(&mut self, p: Page) {
        self.page = p;
        self.page_enter = std::time::Instant::now();
    }

    fn next_page(&self) -> Option<Page> {
        let idx = self.theme.pages.iter().position(|p| *p == self.page)?;
        self.theme.pages.get(idx + 1).copied()
    }

    fn prev_page(&self) -> Option<Page> {
        let idx = self.theme.pages.iter().position(|p| *p == self.page)?;
        if idx == 0 {
            None
        } else {
            self.theme.pages.get(idx - 1).copied()
        }
    }

    fn start_install(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.install_rx = Some(rx);
        self.set_page(Page::Install);

        let manifest = self.manifest.clone();
        let exe = self.self_exe.clone();
        let target = PathBuf::from(self.dir.trim().trim_matches('"').to_string());
        let selected: BTreeSet<String> = self
            .components
            .iter()
            .filter(|(id, on)| {
                **on || manifest
                    .components
                    .iter()
                    .any(|c| &c.id == *id && c.required)
            })
            .map(|(id, _)| id.clone())
            .collect();
        std::thread::spawn(move || {
            // 成功与失败都必须回传 Done——此前只在 Err 时发送，安装成功后
            // GUI 永远等不到完成消息，向导停在 finalize 页（引擎其实已完成）。
            let r = run_install_thread(manifest, exe, target, selected, &tx);
            let _ = tx.send(UiMsg::Done(r));
        });
    }

    fn start_uninstall(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.install_rx = Some(rx);
        let mut manifest = self.manifest.clone();
        if !self.uninstall_keep {
            manifest.uninstall.keep.clear();
        }
        let exe = self.self_exe.clone();
        std::thread::spawn(move || {
            let r = run_uninstall_thread(&manifest, &exe, &tx);
            let _ = tx.send(UiMsg::Done(r));
        });
    }

    fn poll_install(&mut self) {
        // 先取空 channel（块作用域结束即释放不可变借用），再处理消息
        let msgs: Vec<UiMsg> = {
            let Some(rx) = &self.install_rx else { return };
            let mut v = Vec::new();
            while let Ok(m) = rx.try_recv() {
                v.push(m);
            }
            v
        };
        for msg in msgs {
            match msg {
                UiMsg::Step(s) => self.progress.step = s,
                UiMsg::File { path, done, total } => {
                    self.progress.current_file = path;
                    self.progress.files_done = done;
                    self.progress.files_total = total;
                }
                UiMsg::Done(r) => {
                    self.progress.finished = true;
                    self.result = Some(r);
                    self.set_page(Page::Finish);
                }
            }
        }
    }
}

/// 进度订阅者：把事件压缩成 UiMsg 发给 UI。
struct UiProgress {
    tx: Sender<UiMsg>,
    files_total: usize,
    files_done: usize,
}

impl Subscriber for UiProgress {
    fn id(&self) -> &str {
        "ui-progress"
    }
    fn on_event(&mut self, event: &Event, _ctx: &EngineCtx) -> Decision {
        match event {
            Event::BeforeStep(s) => {
                let _ = self.tx.send(UiMsg::Step(s.name().to_string()));
            }
            Event::AfterFile { .. } => {
                self.files_done += 1;
                let _ = self.tx.send(UiMsg::File {
                    path: String::new(),
                    done: self.files_done,
                    total: self.files_total,
                });
            }
            Event::BeforeFile { path } => {
                let _ = self.tx.send(UiMsg::File {
                    path: path.clone(),
                    done: self.files_done,
                    total: self.files_total,
                });
            }
            _ => {}
        }
        Decision::Continue
    }
}

fn run_install_thread(
    manifest: Manifest,
    exe: PathBuf,
    target: PathBuf,
    selected: BTreeSet<String>,
    tx: &Sender<UiMsg>,
) -> Result<(), EngineError> {
    let mut pkg =
        Package::open(&exe).map_err(|e| EngineError::Fatal(format!("解析安装包: {e}")))?;

    let _mutex = misc::SingleInstance::new(&format!("MoInstaller.{}", manifest.app.id))
        .ok_or_else(|| EngineError::Fatal("安装程序已在运行".into()))?;

    let base_env = ConstEnv::from_process_env();
    let env = base_env.with_app(&target, &manifest.app.name);
    let total: usize = pkg.entries.len();
    let selected_vec: Vec<String> = selected.iter().cloned().collect();
    let ctx = EngineCtx {
        app_dir: target.clone(),
        app_name: manifest.app.name.clone(),
        app_id: manifest.app.id.clone(),
        version: manifest.app.version.clone(),
        silent: false,
        selected_components: selected,
        env,
    };
    // 与静默路径一致：L1 钩子 + L2 脚本 + UI 进度
    let script = super::script_source(&manifest, &mut pkg)?;
    let mut bus = super::build_bus(&manifest, false, selected_vec, script)?;
    bus.subscribe(Box::new(UiProgress {
        tx: tx.clone(),
        files_total: total,
        files_done: 0,
    }));
    let mut executor = Executor::new(bus, ctx, target.join("mo-install.log"));
    executor.install(&manifest, &mut pkg, &exe)
}

fn run_uninstall_thread(
    manifest: &Manifest,
    exe: &std::path::Path,
    tx: &Sender<UiMsg>,
) -> Result<(), EngineError> {
    let app_dir = exe
        .parent()
        .ok_or_else(|| EngineError::Fatal("无法定位安装目录".into()))?
        .to_path_buf();
    let env = ConstEnv::from_process_env().with_app(&app_dir, &manifest.app.name);
    let ctx = EngineCtx {
        app_dir: app_dir.clone(),
        app_name: manifest.app.name.clone(),
        app_id: manifest.app.id.clone(),
        version: manifest.app.version.clone(),
        silent: false,
        selected_components: BTreeSet::new(),
        env,
    };
    let mut bus = EventBus::new();
    if !manifest.hooks.is_empty() {
        bus.subscribe(Box::new(HookRunner::new(manifest.hooks.clone())));
    }
    let _ = tx.send(UiMsg::Step("uninstall".to_string()));
    let mut executor = Executor::new(bus, ctx, app_dir.join("mo-install.log"));
    executor.uninstall(manifest, exe)
}

/// 运行安装 GUI。返回 (是否成功, 成功后要运行的程序（已展开）)。
/// 安装系统中文字体：egui 内置字体不含 CJK 字形，中文文案会渲染为豆腐块。
/// 按优先级探测 Windows 常见中文字体（ttc 取 index 0），全部缺失则保持默认。
fn install_cjk_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for cand in [
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/simsun.ttc",
    ] {
        if let Ok(bytes) = std::fs::read(cand) {
            fonts.font_data.insert(
                "cjk".into(),
                std::sync::Arc::new(egui::FontData::from_owned(bytes)),
            );
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                if let Some(list) = fonts.families.get_mut(&family) {
                    list.push("cjk".into());
                }
            }
            break;
        }
    }
    ctx.set_fonts(fonts);
}

pub fn run_install_gui(mut app: WizardApp) -> (bool, Option<String>) {
    let mut native = eframe::NativeOptions::default();
    let (w, h) = app
        .manifest
        .theme
        .window
        .as_ref()
        .map(|w| (w.width as f32, w.height as f32))
        .unwrap_or((800.0, 560.0));
    native.viewport.inner_size = Some(egui::vec2(w, h));
    native.viewport.min_inner_size = Some(egui::vec2(680.0, 520.0));
    let title = format!(
        "{} - {}",
        app.manifest.app.name,
        app.theme.tr("wizard.title")
    );
    let outcome = std::rc::Rc::new(std::cell::RefCell::new((false, None::<String>)));
    let out2 = outcome.clone();
    let _ = eframe::run_native(
        &title,
        native,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            install_cjk_fonts(&ctx);
            // 用真实 ctx 重建主题（纹理）
            let theme = ThemeRuntime::new(
                &app.manifest,
                &ctx,
                app.theme.lang,
                app.banner_bytes.take(),
                app.sidebar_bytes.take(),
            );
            let pages = theme.pages.clone();
            app.theme = theme;
            if !app.uninstall_mode && !pages.contains(&app.page) {
                app.page = pages.first().copied().unwrap_or(Page::Install);
            }
            // 有效页面序列直接以 Install 开头时（如 pages = ["install",
            // "finish"]），Install 页没有「下一步」可点——直接启动安装。
            if !app.uninstall_mode && app.page == Page::Install && app.install_rx.is_none() {
                app.start_install();
            }
            Ok(Box::new(GuiWrap {
                app,
                outcome: GuiOutcome::Install(out2),
            }))
        }),
    );
    let (ok, prog) = outcome.borrow().clone();
    (ok, prog)
}

/// 运行卸载 GUI。返回是否成功。
pub fn run_uninstall_gui(mut app: WizardApp) -> bool {
    let mut native = eframe::NativeOptions::default();
    native.viewport.inner_size = Some(egui::vec2(600.0, 380.0));
    native.viewport.min_inner_size = Some(egui::vec2(520.0, 340.0));
    let title = format!(
        "{} - {}",
        app.manifest.app.name,
        app.theme.tr("wizard.uninstall.title")
    );
    let outcome = std::rc::Rc::new(std::cell::RefCell::new(false));
    let out2 = outcome.clone();
    let _ = eframe::run_native(
        &title,
        native,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            install_cjk_fonts(&ctx);
            let theme = ThemeRuntime::new(
                &app.manifest,
                &ctx,
                app.theme.lang,
                app.banner_bytes.take(),
                app.sidebar_bytes.take(),
            );
            app.theme = theme;
            Ok(Box::new(GuiWrap {
                app,
                outcome: GuiOutcome::Uninstall(out2),
            }))
        }),
    );
    *outcome.borrow()
}

enum GuiOutcome {
    Install(std::rc::Rc<std::cell::RefCell<(bool, Option<String>)>>),
    Uninstall(std::rc::Rc<std::cell::RefCell<bool>>),
}

struct GuiWrap {
    app: WizardApp,
    outcome: GuiOutcome,
}

impl eframe::App for GuiWrap {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.poll_install();
        // 工作线程运行中持续请求重绘：egui 默认被动渲染，否则进度与
        // 完成消息要等下一次鼠标/键盘事件才会被轮询。
        if self.app.running() {
            ctx.request_repaint();
        }
        // 运行中拦截原生窗口关闭（X），防止杀死工作线程留下半安装。
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && self.app.running() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.app.theme.apply_style(ctx);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::WHITE))
            .show(ctx, |ui| {
                self.app.ui(ui);
            });
        if self.app.closed {
            match &self.outcome {
                GuiOutcome::Install(o) => {
                    let ok = matches!(self.app.result, Some(Ok(())));
                    let prog = if ok && self.app.run_after {
                        self.app
                            .manifest
                            .run
                            .after
                            .as_deref()
                            .and_then(|p| self.app.theme_env().expand(p).ok())
                    } else {
                        None
                    };
                    *o.borrow_mut() = (ok, prog);
                }
                GuiOutcome::Uninstall(o) => {
                    *o.borrow_mut() = matches!(self.app.result, Some(Ok(())));
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

/// 白色「下载入托盘」符号（安装意象），c 为符号中心，s 为比例（1.0 = 72px logo 尺度）。
fn draw_symbol(painter: &egui::Painter, c: egui::Pos2, s: f32, color: Color32) {
    // 箭头杆
    painter.rect_filled(
        egui::Rect::from_center_size(c + egui::vec2(0.0, -8.0 * s), egui::vec2(7.0 * s, 16.0 * s)),
        egui::CornerRadius::same(3),
        color,
    );
    // 箭头 V 尖
    let tip = c + egui::vec2(0.0, 8.0 * s);
    painter.line_segment(
        [c + egui::vec2(-11.0 * s, -6.0 * s), tip],
        egui::Stroke::new(7.0_f32 * s, color),
    );
    painter.line_segment(
        [tip, c + egui::vec2(11.0 * s, -6.0 * s)],
        egui::Stroke::new(7.0_f32 * s, color),
    );
    // 托盘
    painter.rect_filled(
        egui::Rect::from_center_size(c + egui::vec2(0.0, 22.0 * s), egui::vec2(30.0 * s, 5.0 * s)),
        egui::CornerRadius::same(2),
        color,
    );
}

/// 品牌 logo：双层叠块（亮色底块错位 + accent 主块）+ 白色安装符号。
fn draw_install_logo(ui: &mut egui::Ui, rect: egui::Rect, accent: Color32) {
    let painter = ui.painter();
    let corner = 16.0_f32;
    let light = Color32::from_rgb(
        (accent.r() as u32 + (255 - accent.r() as u32) * 45 / 100) as u8,
        (accent.g() as u32 + (255 - accent.g() as u32) * 45 / 100) as u8,
        (accent.b() as u32 + (255 - accent.b() as u32) * 45 / 100) as u8,
    );
    painter.rect_filled(
        rect.translate(egui::vec2(6.0, 6.0)),
        egui::CornerRadius::same(corner as u8),
        light,
    );
    painter.rect_filled(rect, egui::CornerRadius::same(corner as u8), accent);
    draw_symbol(painter, rect.center(), 1.0, Color32::WHITE);
}

/// 圆环进度：浅色轨道 + accent 弧 + 端点光斑，中心为百分比大字。
fn draw_progress_ring(ui: &mut egui::Ui, rect: egui::Rect, frac: f32, accent: Color32, pct: i32) {
    let painter = ui.painter();
    let center = rect.center();
    let radius = 62.0_f32;
    let lw = 12.0_f32;
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(lw, Color32::from_rgb(0xEC, 0xEF, 0xF3)),
    );
    if frac > 0.001 {
        let start = -std::f32::consts::FRAC_PI_2;
        let end = start + frac * std::f32::consts::TAU;
        // 弧离散为折线（粗圆帽线段串，视觉即平滑弧）
        let n = ((frac * 64.0).ceil() as usize).max(2);
        let points: Vec<egui::Pos2> = (0..=n)
            .map(|i| {
                let a = start + (end - start) * (i as f32 / n as f32);
                center + egui::vec2(a.cos(), a.sin()) * radius
            })
            .collect();
        painter.add(egui::Shape::line(points, egui::Stroke::new(lw, accent)));
        let tip = center + egui::vec2(end.cos(), end.sin()) * radius;
        painter.circle_filled(
            tip,
            lw * 0.5 + 3.0,
            Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 70),
        );
        painter.circle_filled(tip, 4.5, Color32::WHITE);
    }
    painter.text(
        center,
        egui::Align2::CENTER_CENTER,
        format!("{pct}%"),
        egui::FontId::proportional(30.0),
        accent,
    );
}

/// 成功/失败大徽章：圆形底色 + 对勾/叉，k 为入场进度（0..1，带回弹）。
fn badge(ui: &mut egui::Ui, ok: bool, k: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), Sense::hover());
    let (fill, mark) = if ok {
        (Color32::from_rgb(0x2E, 0xA0, 0x4E), "\u{2713}")
    } else {
        (Color32::from_rgb(0xD8, 0x3A, 0x3A), "\u{2715}")
    };
    // ease-out-back：轻微过冲的弹性入场
    let c1 = 1.70158;
    let e = 1.0 + (c1 + 1.0) * (k - 1.0).powi(3) + c1 * (k - 1.0).powi(2);
    let e = e.clamp(0.0, 1.15);
    let center = rect.center();
    let halo = 34.0 * e;
    // 光晕
    ui.painter().circle_filled(
        center,
        halo,
        Color32::from_rgba_unmultiplied(fill.r(), fill.g(), fill.b(), 38),
    );
    ui.painter().circle_filled(center, 28.0 * e, fill);
    ui.painter().text(
        center,
        egui::Align2::CENTER_CENTER,
        mark,
        egui::FontId::proportional(30.0),
        Color32::WHITE,
    );
}

impl WizardApp {
    fn theme_env(&self) -> ConstEnv {
        ConstEnv::from_process_env()
            .with_app(std::path::Path::new(&self.dir), &self.manifest.app.name)
    }

    fn ui(&mut self, ui: &mut egui::Ui) {
        // —— 左侧品牌栏（卸载窗口不显示）——
        let side_w = if self.uninstall_mode { 0.0 } else { 176.0 };
        egui::SidePanel::left("mo-sidebar")
            .exact_width(side_w)
            .frame(egui::Frame::NONE.fill(self.theme.sidebar_fill()))
            .show_inside(ui, |ui| {
                if side_w > 0.0 {
                    self.sidebar(ui);
                }
            });

        // —— 底部按钮条：白底 + 上分隔线 ——
        egui::TopBottomPanel::bottom("mo-buttons")
            .exact_height(60.0)
            .frame(egui::Frame::NONE.fill(Color32::WHITE))
            .show_inside(ui, |ui| {
                let r = ui.available_rect_before_wrap();
                ui.painter().hline(
                    r.left()..=r.right(),
                    r.top(),
                    egui::Stroke::new(1.0_f32, Color32::from_rgb(0xE4, 0xE7, 0xEC)),
                );
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add_space(14.0);
                    self.buttons(ui);
                });
            });

        // —— 中央内容区 ——
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(Color32::WHITE))
            .show_inside(ui, |ui| {
                // banner（包内提供时）：cover 铺满、封顶 160px
                if let Some(tex) = self.theme.banner.clone() {
                    let size = tex.size_vec2();
                    let w = ui.available_width();
                    let scale = w / size.x;
                    let full_h = size.y * scale;
                    let h = full_h.min(160.0);
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), Sense::hover());
                    let uv = if full_h > h {
                        let cut = (1.0 - h / full_h) / 2.0;
                        egui::Rect::from_min_max(egui::pos2(0.0, cut), egui::pos2(1.0, 1.0 - cut))
                    } else {
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0))
                    };
                    ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
                    ui.add_space(10.0);
                }
                egui::Frame::NONE
                    .inner_margin(egui::Margin {
                        left: 30,
                        right: 30,
                        top: 24,
                        bottom: 12,
                    })
                    .show(ui, |ui| {
                        self.page_body(ui);
                    });
            });
    }

    /// 品牌栏：应用侧图铺满；否则手绘（首字母徽标 + 名称/版本/发行商）。
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        if let Some(tex) = self.theme.sidebar.clone() {
            let rect = ui.available_rect_before_wrap();
            ui.allocate_rect(rect, Sense::hover());
            let size = tex.size_vec2();
            let scale = (rect.width() / size.x).max(rect.height() / size.y);
            let disp = size * scale;
            let r = egui::Rect::from_center_size(rect.center(), disp);
            ui.painter().image(
                tex.id(),
                r,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            return;
        }
        let name = self.manifest.app.name.clone();
        let version = self.manifest.app.version.clone();
        let publisher = self.manifest.app.publisher.clone();

        ui.add_space(30.0);
        // 玻璃感 logo：半透明白双层块 + 白色安装符号
        ui.horizontal(|ui| {
            ui.add_space(22.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(52.0, 52.0), Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(
                rect.translate(egui::vec2(4.0, 4.0)),
                egui::CornerRadius::same(13),
                white_a(0.10),
            );
            painter.rect_filled(rect, egui::CornerRadius::same(13), white_a(0.18));
            draw_symbol(painter, rect.center(), 0.72, Color32::WHITE);
        });
        ui.add_space(18.0);
        ui.horizontal(|ui| {
            ui.add_space(22.0);
            ui.vertical(|ui| {
                ui.set_max_width(128.0);
                ui.label(
                    RichText::new(&name)
                        .color(Color32::WHITE)
                        .size(17.0)
                        .strong(),
                );
                ui.add_space(3.0);
                ui.label(
                    RichText::new(format!("v{version}"))
                        .color(white_a(0.72))
                        .size(12.5),
                );
                if !publisher.is_empty() {
                    ui.add_space(2.0);
                    ui.label(RichText::new(&publisher).color(white_a(0.58)).size(12.0));
                }
            });
        });
        ui.with_layout(Layout::bottom_up(egui::Align::LEFT), |ui| {
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.add_space(22.0);
                ui.label(
                    RichText::new("Powered by MoInstaller")
                        .color(white_a(0.42))
                        .size(11.0),
                );
            });
        });
    }

    fn buttons(&mut self, ui: &mut egui::Ui) {
        let cancel_label = self.theme.tr("wizard.btn.cancel");
        let back_label = self.theme.tr("wizard.btn.back");
        let next_label_next = self.theme.tr("wizard.btn.next");
        let next_label_install = self.theme.tr("wizard.btn.install");
        let next_label_uninstall = self.theme.tr("wizard.btn.uninstall");
        match self.page {
            Page::Install if !self.uninstall_mode => {
                // 安装进行中禁止取消：直接退出会杀死工作线程，留下无日志的半安装。
                if ui
                    .add_enabled(!self.running(), self.theme.secondary_button(&cancel_label))
                    .clicked()
                {
                    self.closed = true;
                }
            }
            Page::Finish => {
                if self.uninstall_mode && self.result.is_none() {
                    let busy = self.running();
                    if ui
                        .add_enabled(!busy, self.theme.primary_button(&next_label_uninstall))
                        .clicked()
                    {
                        self.start_uninstall();
                    }
                    if ui
                        .add_enabled(!busy, self.theme.secondary_button(&cancel_label))
                        .clicked()
                    {
                        self.closed = true;
                    }
                } else {
                    let close_label = self.theme.tr("wizard.btn.close");
                    if ui.add(self.theme.primary_button(&close_label)).clicked() {
                        self.closed = true;
                    }
                }
            }
            _ => {
                let next_label = match self.next_page() {
                    Some(Page::Install) if !self.uninstall_mode => next_label_install.clone(),
                    Some(Page::Install) => next_label_uninstall.clone(),
                    _ => next_label_next.clone(),
                };
                let can_next = self.can_proceed();
                let clicked_next = ui
                    .add_enabled(can_next, self.theme.primary_button(&next_label))
                    .clicked();
                if clicked_next {
                    if self.next_page() == Some(Page::Install) && !self.uninstall_mode {
                        self.start_install();
                    } else if let Some(p) = self.next_page() {
                        self.set_page(p);
                    }
                }
                if ui.add(self.theme.secondary_button(&back_label)).clicked()
                    && let Some(p) = self.prev_page()
                {
                    self.set_page(p);
                }
                if ui.add(self.theme.secondary_button(&cancel_label)).clicked() {
                    self.closed = true;
                }
            }
        }
    }

    fn can_proceed(&self) -> bool {
        match self.page {
            Page::License => self.license_accepted,
            Page::Dir => !self.dir.trim().is_empty(),
            _ => true,
        }
    }

    fn page_body(&mut self, ui: &mut egui::Ui) {
        // 页面入场：0.25s ease-out 淡入，动画未结束期间持续请求重绘
        let t = self.page_enter.elapsed().as_secs_f32();
        let k = (t / 0.25).min(1.0);
        let k = 1.0 - (1.0 - k) * (1.0 - k);
        if k < 1.0 {
            ui.ctx().request_repaint();
        }
        ui.set_opacity(k);
        let accent = self.theme.accent;
        match self.page {
            Page::Welcome => {
                let title = self.theme.tr("wizard.welcome.title");
                let text = self.theme.tr("wizard.welcome.text");
                let hint = self.theme.tr("wizard.welcome.hint");
                let name = self.manifest.app.name.clone();
                let version = self.manifest.app.version.clone();
                let publisher = self.manifest.app.publisher.clone();

                // 大 logo（双层叠块 + 安装符号），入场带上移
                let drift = (1.0 - k) * 14.0;
                let (lrect, _) = ui.allocate_exact_size(egui::vec2(72.0, 72.0), Sense::hover());
                let lrect = lrect.translate(egui::vec2(0.0, -drift));
                draw_install_logo(ui, lrect, accent);
                ui.add_space(16.0);
                ui.heading(RichText::new(&title).strong());
                ui.add_space(12.0);
                ui.label(
                    RichText::new(format!("{name} {version}"))
                        .size(19.0)
                        .strong()
                        .color(accent),
                );
                if !publisher.is_empty() {
                    ui.label(RichText::new(&publisher).weak());
                }
                ui.add_space(10.0);
                ui.label(RichText::new(&text).size(15.0).color(TEXT_SOFT));
                ui.add_space(22.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new(&hint).weak());
            }
            Page::License => {
                let title = self.theme.tr("wizard.license.title");
                let prompt = self.theme.tr("wizard.license.prompt");
                let accept = self.theme.tr("wizard.license.accept");
                ui.heading(RichText::new(&title).strong());
                ui.add_space(6.0);
                ui.label(RichText::new(&prompt).weak());
                ui.add_space(12.0);
                let avail = ui.available_height() - 60.0;
                egui::ScrollArea::vertical()
                    .id_salt("mo-license-scroll")
                    .max_height(avail.max(80.0))
                    .show(ui, |ui| {
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(0xFA, 0xFB, 0xFC))
                            .stroke(egui::Stroke::new(
                                1.0_f32,
                                Color32::from_rgb(0xE2, 0xE6, 0xEC),
                            ))
                            .corner_radius(egui::CornerRadius::same(8))
                            .inner_margin(egui::Margin::same(12))
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut self.license_text.as_str())
                                        .desired_width(f32::INFINITY)
                                        .frame(false)
                                        .interactive(false),
                                );
                            });
                    });
                ui.add_space(10.0);
                ui.checkbox(&mut self.license_accepted, &accept);
            }
            Page::Dir => {
                let title = self.theme.tr("wizard.dir.title");
                let prompt = self.theme.tr("wizard.dir.prompt");
                let browse = self.theme.tr("wizard.dir.browse");
                let free_lbl = self.theme.tr("wizard.dir.freespace");
                let to_lbl = self.theme.tr("wizard.dir.installto");
                ui.heading(RichText::new(&title).strong());
                ui.add_space(6.0);
                ui.label(RichText::new(&prompt).weak());
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    let tw = ui.available_width() - 110.0;
                    ui.add_sized(
                        [tw, 36.0],
                        egui::TextEdit::singleline(&mut self.dir).clip_text(true),
                    );
                    if ui.add(self.theme.secondary_button(&browse)).clicked()
                        && let Some(p) = picker::pick_folder()
                    {
                        // 浏览选中的目录非空时追加应用名子目录，避免把文件散落进
                        // 既有目录；空目录（含尚不存在）按用户所选原样使用。
                        // 升级场景不受影响：默认目录不经此处改写。
                        let target = if dir_has_content(&p) {
                            p.join(sanitize_dir_name(&self.manifest.app.name))
                        } else {
                            p
                        };
                        self.dir = target.to_string_lossy().into_owned();
                    }
                });
                ui.add_space(14.0);
                let free = misc::disk_free_bytes(std::path::Path::new(&self.dir)).ok();
                let dir_disp = self.dir.clone();
                egui::Frame::NONE
                    .fill(Color32::from_rgb(0xF7, 0xF8, 0xFA))
                    .corner_radius(egui::CornerRadius::same(8))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        if let Some(f) = free {
                            ui.label(RichText::new(format!("{free_lbl}: {}", human(f))).size(13.5));
                        }
                        ui.label(
                            RichText::new(format!("{to_lbl}: {dir_disp}"))
                                .size(13.5)
                                .weak(),
                        );
                    });
            }
            Page::Components => {
                let title = self.theme.tr("wizard.components.title");
                let prompt = self.theme.tr("wizard.components.prompt");
                let req_lbl = self.theme.tr("wizard.components.required");
                ui.heading(RichText::new(&title).strong());
                ui.add_space(6.0);
                ui.label(RichText::new(&prompt).weak());
                ui.add_space(12.0);
                let comps: Vec<(String, String, bool, bool)> = self
                    .manifest
                    .components
                    .iter()
                    .map(|c| (c.id.clone(), c.name.clone(), c.required, c.default))
                    .collect();
                for (id, name, required, default) in comps {
                    ui.horizontal(|ui| {
                        let on = self.components.entry(id).or_insert(default);
                        let mut v = *on;
                        ui.add_enabled(!required, egui::Checkbox::new(&mut v, ""));
                        *on = v || required;
                        ui.label(RichText::new(&name).strong().size(14.5));
                        if required {
                            ui.label(RichText::new(&req_lbl).weak().size(12.0));
                        }
                    });
                    ui.add_space(4.0);
                }
            }
            Page::Install => {
                let title = self.theme.tr("wizard.install.title");
                let wait = self.theme.tr("wizard.install.wait");
                ui.heading(RichText::new(&title).strong());
                ui.add_space(4.0);
                ui.label(RichText::new(&wait).weak());
                ui.add_space(20.0);
                let frac = if self.progress.files_total > 0 {
                    self.progress.files_done as f32 / self.progress.files_total as f32
                } else {
                    0.0
                };
                // 进度平滑追踪（0.4s 缓动）
                let shown = ui.ctx().animate_value_with_time(
                    egui::Id::new("mo-progress"),
                    frac.clamp(0.0, 1.0),
                    0.4,
                );
                let pct = (shown * 100.0).round() as i32;

                // 圆环进度
                ui.vertical_centered(|ui| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(148.0, 148.0), Sense::hover());
                    draw_progress_ring(ui, rect, shown, accent, pct);
                });
                ui.add_space(18.0);

                let step = self.progress.step.clone();
                let file = self.progress.current_file.clone();
                let done = self.progress.files_done;
                let total = self.progress.files_total;
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(18.0).color(accent));
                    ui.label(RichText::new(&step).strong().size(13.5));
                });
                ui.add(
                    egui::Label::new(
                        RichText::new(format!("{file}  ({done} / {total})"))
                            .weak()
                            .size(12.0)
                            .monospace(),
                    )
                    .truncate(),
                );
            }
            Page::Finish if !self.uninstall_mode => match &self.result {
                Some(Ok(())) => {
                    let title = self.theme.tr("wizard.finish.title");
                    let text = self.theme.tr("wizard.finish.text");
                    badge(ui, true, k);
                    ui.add_space(14.0);
                    ui.heading(RichText::new(&title).strong());
                    ui.add_space(6.0);
                    ui.label(RichText::new(&text).size(15.0).color(TEXT_SOFT));
                    if self.manifest.run.after.is_some() {
                        ui.add_space(12.0);
                        let run_lbl = self.theme.tr("wizard.finish.run");
                        let name = self.manifest.app.name.clone();
                        ui.checkbox(&mut self.run_after, format!("{run_lbl} {name}"));
                    }
                }
                Some(Err(e)) => {
                    let failed = self.theme.tr("wizard.finish.failed");
                    badge(ui, false, k);
                    ui.add_space(14.0);
                    ui.heading(RichText::new(&failed).strong());
                    ui.add_space(8.0);
                    let msg = e.message().to_string();
                    egui::Frame::NONE
                        .fill(Color32::from_rgb(0xFC, 0xEC, 0xEC))
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(12))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new(&msg)
                                    .size(13.0)
                                    .color(Color32::from_rgb(0x9E, 0x2B, 0x2B)),
                            );
                        });
                }
                None => {
                    let title = self.theme.tr("wizard.install.title");
                    ui.heading(RichText::new(&title).strong());
                }
            },
            Page::Finish => {
                // 卸载模式
                match &self.result {
                    Some(Ok(())) => {
                        let title = self.theme.tr("wizard.uninstall.title");
                        let done = self.theme.tr("wizard.uninstall.done");
                        badge(ui, true, k);
                        ui.add_space(14.0);
                        ui.heading(RichText::new(&title).strong());
                        ui.add_space(6.0);
                        ui.label(RichText::new(&done).size(15.0).color(TEXT_SOFT));
                    }
                    Some(Err(e)) => {
                        let failed = self.theme.tr("wizard.finish.failed");
                        badge(ui, false, k);
                        ui.add_space(14.0);
                        ui.heading(RichText::new(&failed).strong());
                        ui.add_space(8.0);
                        let msg = e.message().to_string();
                        egui::Frame::NONE
                            .fill(Color32::from_rgb(0xFC, 0xEC, 0xEC))
                            .corner_radius(egui::CornerRadius::same(8))
                            .inner_margin(egui::Margin::same(12))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new(&msg)
                                        .size(13.0)
                                        .color(Color32::from_rgb(0x9E, 0x2B, 0x2B)),
                                );
                            });
                    }
                    None => {
                        let title = self.theme.tr("wizard.uninstall.title");
                        let confirm = self.theme.tr("wizard.uninstall.confirm");
                        let keep = self.theme.tr("wizard.uninstall.keepdata");
                        ui.heading(RichText::new(&title).strong());
                        ui.add_space(10.0);
                        ui.label(RichText::new(&confirm).size(15.0).color(TEXT_SOFT));
                        ui.add_space(14.0);
                        ui.checkbox(&mut self.uninstall_keep, &keep);
                    }
                }
            }
        }
    }
}

fn human(n: u64) -> String {
    if n >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", n as f64 / 1073741824.0)
    } else if n >= 1024 * 1024 {
        format!("{:.1} MB", n as f64 / 1048576.0)
    } else if n >= 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{n} B")
    }
}

/// 应用名清洗为合法目录名：去除 Windows 路径非法字符，空白兜底为 App。
fn sanitize_dir_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        "App".to_string()
    } else {
        trimmed.to_string()
    }
}

/// 目录存在且至少含一个条目（不存在或空返回 false）。
fn dir_has_content(p: &std::path::Path) -> bool {
    std::fs::read_dir(p)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_illegal_chars() {
        assert_eq!(sanitize_dir_name("My App: <v2>?"), "My App v2");
        assert_eq!(sanitize_dir_name("示例应用"), "示例应用");
    }

    #[test]
    fn sanitize_empty_fallback() {
        assert_eq!(sanitize_dir_name("***"), "App");
        assert_eq!(sanitize_dir_name("   "), "App");
    }

    #[test]
    fn dir_content_check() {
        let empty = tempfile::tempdir().unwrap();
        assert!(!dir_has_content(empty.path()));
        let full = tempfile::tempdir().unwrap();
        std::fs::write(full.path().join("x.txt"), b"x").unwrap();
        assert!(dir_has_content(full.path()));
        assert!(!dir_has_content(std::path::Path::new(
            "Z:/definitely/not/exist"
        )));
    }
}
