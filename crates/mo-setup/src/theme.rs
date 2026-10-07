//! 主题运行时：accent 色板、横幅/侧图纹理、文案覆盖、有效页面序列，
//! 以及浅色现代样式（字号层级、圆角、按钮外观）。

use crate::strings::{Lang, builtin};
use egui::{Color32, Context, RichText, TextureHandle, TextureOptions};
use mo_core::manifest::Manifest;
use std::collections::BTreeMap;

/// 向导页（解析自 theme.pages）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Welcome,
    License,
    Dir,
    Components,
    Install,
    Finish,
}

impl Page {
    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "welcome" => Page::Welcome,
            "license" => Page::License,
            "dir" => Page::Dir,
            "components" => Page::Components,
            "install" => Page::Install,
            "finish" => Page::Finish,
            _ => return None,
        })
    }
}

pub struct ThemeRuntime {
    pub lang: Lang,
    pub accent: Color32,
    pub banner: Option<TextureHandle>,
    pub sidebar: Option<TextureHandle>,
    pub strings: BTreeMap<String, String>,
    /// 有效页面序列（显隐/顺序已应用）。
    pub pages: Vec<Page>,
}

impl ThemeRuntime {
    /// 从清单与包内资源构造。banner/sidebar_bytes 为 __mo__/ 资源原始字节。
    pub fn new(
        manifest: &Manifest,
        ctx: &Context,
        lang: Lang,
        banner_bytes: Option<Vec<u8>>,
        sidebar_bytes: Option<Vec<u8>>,
    ) -> Self {
        let accent = manifest
            .theme
            .accent
            .as_deref()
            .and_then(parse_hex_color)
            .unwrap_or(Color32::from_rgb(0x2E, 0x7C, 0xF6));

        let banner = banner_bytes.and_then(|b| load_texture(ctx, b));
        let sidebar = sidebar_bytes.and_then(|b| load_texture(ctx, b));

        // 页面序列：page_order 显式覆盖 > pages；再减 hide_pages；
        // license 页无许可文件则隐藏；components 无组件则隐藏
        let has_license = manifest.options.license.is_some();
        let has_components = !manifest.components.is_empty();
        let hidden: Vec<String> = manifest.theme.hide_pages.clone();
        let order: Vec<String> = if manifest.theme.page_order.is_empty() {
            manifest.theme.pages.clone()
        } else {
            manifest.theme.page_order.clone()
        };
        let mut pages = Vec::new();
        for name in &order {
            if hidden.iter().any(|h| h == name) {
                continue;
            }
            if name == "license" && !has_license {
                continue;
            }
            if name == "components" && !has_components {
                continue;
            }
            if let Some(p) = Page::from_name(name) {
                pages.push(p);
            }
        }
        // 兜底：至少有安装与完成页
        if !pages.contains(&Page::Install) {
            pages.push(Page::Install);
        }
        if !pages.contains(&Page::Finish) {
            pages.push(Page::Finish);
        }

        Self {
            lang,
            accent,
            banner,
            sidebar,
            strings: manifest.theme.strings.clone(),
            pages,
        }
    }

    /// 文案：theme.strings 覆盖 > 内置（lang）> key 本身。
    pub fn tr(&self, key: &str) -> String {
        if let Some(s) = self.strings.get(key) {
            return s.clone();
        }
        builtin(key, self.lang).unwrap_or(key).to_string()
    }

    /// 应用主题：浅色现代风、accent 高亮、字号层级与宽松间距。
    pub fn apply_style(&self, ctx: &Context) {
        let a = self.accent;
        let mut st = (*ctx.style()).clone();

        st.text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(24.0));
        st.text_styles
            .insert(egui::TextStyle::Body, egui::FontId::proportional(15.0));
        st.text_styles
            .insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        st.text_styles
            .insert(egui::TextStyle::Small, egui::FontId::proportional(12.5));
        st.text_styles
            .insert(egui::TextStyle::Monospace, egui::FontId::monospace(13.0));

        st.spacing.item_spacing = egui::vec2(10.0, 10.0);
        st.spacing.button_padding = egui::vec2(18.0, 8.0);

        let v = &mut st.visuals;
        *v = egui::Visuals::light();
        v.panel_fill = Color32::WHITE;
        v.window_fill = Color32::WHITE;
        v.extreme_bg_color = Color32::from_rgb(0xF7, 0xF8, 0xFA);
        v.faint_bg_color = Color32::from_rgb(0xF2, 0xF4, 0xF7);
        v.hyperlink_color = a;
        v.selection.bg_fill = a;
        v.selection.stroke = egui::Stroke::new(1.0_f32, Color32::WHITE);
        v.error_fg_color = Color32::from_rgb(0xC6, 0x28, 0x28);

        let border = egui::Stroke::new(1.0_f32, Color32::from_rgb(0xD9, 0xDE, 0xE5));
        let text = egui::Stroke::new(1.2_f32, Color32::from_rgb(0x37, 0x3C, 0x44));
        for w in [
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
        ] {
            w.bg_fill = Color32::WHITE;
            w.bg_stroke = border;
            w.fg_stroke = text;
            w.corner_radius = egui::CornerRadius::same(7);
        }
        v.widgets.hovered.bg_fill = tint(a, 0.12);
        v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, a);
        v.widgets.hovered.fg_stroke = egui::Stroke::new(1.2_f32, a);
        v.widgets.active.bg_fill = tint(a, 0.20);
        v.widgets.active.bg_stroke = egui::Stroke::new(1.2_f32, a);
        v.widgets.active.fg_stroke = egui::Stroke::new(1.2_f32, a);
        v.widgets.noninteractive.bg_fill = Color32::WHITE;
        v.widgets.noninteractive.bg_stroke = egui::Stroke::NONE;
        v.widgets.noninteractive.fg_stroke =
            egui::Stroke::new(1.1_f32, Color32::from_rgb(0x5A, 0x61, 0x6B));
        v.widgets.noninteractive.corner_radius = egui::CornerRadius::same(7);

        ctx.set_style(st);
    }

    /// 主按钮：accent 填充白字，向导主操作（下一步/安装/关闭）。
    pub fn primary_button(&self, label: impl Into<String>) -> egui::Button<'static> {
        egui::Button::new(RichText::new(label.into()).color(Color32::WHITE).strong())
            .fill(self.accent)
            .stroke(egui::Stroke::NONE)
            .min_size(egui::vec2(118.0, 36.0))
            .corner_radius(egui::CornerRadius::same(7))
    }

    /// 次按钮：白底浅描边（外观来自 apply_style）。
    pub fn secondary_button(&self, label: impl Into<String>) -> egui::Button<'static> {
        egui::Button::new(RichText::new(label.into())).min_size(egui::vec2(96.0, 36.0))
    }

    /// 侧栏底色：accent 压暗，保证白字可读。
    pub fn sidebar_fill(&self) -> Color32 {
        let a = self.accent;
        Color32::from_rgb(
            (a.r() as u32 * 45 / 100) as u8,
            (a.g() as u32 * 45 / 100) as u8,
            (a.b() as u32 * 45 / 100) as u8,
        )
    }
}

fn parse_hex_color(s: &str) -> Option<Color32> {
    let s = s.trim();
    if s.len() != 7 || !s.starts_with('#') {
        return None;
    }
    let hex = &s[1..];
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Color32::from_rgb(r, g, b))
}

fn load_texture(ctx: &Context, bytes: Vec<u8>) -> Option<TextureHandle> {
    let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &img);
    Some(ctx.load_texture("mo-theme", color, TextureOptions::default()))
}

/// accent 的半透明版（白底上呈浅色高亮）。
fn tint(c: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), (alpha * 255.0) as u8)
}
