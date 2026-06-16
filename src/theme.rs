use eframe::egui::{self, Color32, Margin, Rounding, Shadow, Stroke};

// ── Palette ──────────────────────────────────────────────────────────────────
pub const BG:    Color32 = Color32::from_rgb(0x0d, 0x0d, 0x1a);
pub const SURF:  Color32 = Color32::from_rgb(0x18, 0x18, 0x26);
pub const SURF2: Color32 = Color32::from_rgb(0x20, 0x20, 0x3a);
pub const SURF3: Color32 = Color32::from_rgb(0x2c, 0x2c, 0x4a);
pub const ACC:   Color32 = Color32::from_rgb(0x7c, 0x6f, 0xf0);
pub const ACC2:  Color32 = Color32::from_rgb(0xb0, 0xa8, 0xff);
pub const TXT:   Color32 = Color32::from_rgb(0xdc, 0xe0, 0xf5);
pub const TDIM:  Color32 = Color32::from_rgb(0x80, 0x80, 0xaa);
pub const TGOOD: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);
pub const TERR:  Color32 = Color32::from_rgb(0xf8, 0x71, 0x71);
pub const TWARN: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
pub const BDR:   Color32 = Color32::from_rgb(0x38, 0x38, 0x6a);
pub const ROW_ALT: Color32 = Color32::from_rgb(0x22, 0x22, 0x3f);

// ── Helpers ───────────────────────────────────────────────────────────────────
pub fn section_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(SURF2)
        .rounding(Rounding::same(6.0))
        .stroke(Stroke::new(1.0, BDR))
        .inner_margin(Margin::same(12.0))
}

// ── Style setup ───────────────────────────────────────────────────────────────
pub fn setup_style(ctx: &egui::Context) {
    let mut vis = egui::Visuals::dark();

    vis.override_text_color = Some(TXT);
    vis.panel_fill           = SURF;
    vis.window_fill          = SURF;
    vis.extreme_bg_color     = BG;
    vis.faint_bg_color       = SURF2;
    vis.code_bg_color        = SURF2;
    vis.window_stroke        = Stroke::new(1.0, BDR);
    vis.window_shadow        = Shadow::NONE;
    vis.popup_shadow         = Shadow::NONE;
    vis.window_rounding      = Rounding::same(8.0);
    vis.menu_rounding        = Rounding::same(6.0);
    vis.hyperlink_color      = ACC2;
    vis.warn_fg_color        = TWARN;
    vis.error_fg_color       = TERR;

    // Widget states
    macro_rules! wset {
        ($w:expr, fill=$f:expr, stroke=$s:expr, fg=$fg:expr) => {
            $w.bg_fill    = $f;
            $w.bg_stroke  = Stroke::new(1.0, $s);
            $w.fg_stroke  = Stroke::new(1.5, $fg);
            $w.rounding   = Rounding::same(4.0);
            $w.expansion  = 0.0;
        };
    }
    wset!(vis.widgets.noninteractive, fill=SURF2,  stroke=BDR,  fg=TDIM);
    wset!(vis.widgets.inactive,       fill=SURF2,  stroke=BDR,  fg=TXT);
    wset!(vis.widgets.hovered,        fill=SURF3,  stroke=ACC2, fg=TXT);
    wset!(vis.widgets.active,         fill=ACC,    stroke=ACC2, fg=Color32::WHITE);
    wset!(vis.widgets.open,           fill=SURF3,  stroke=ACC,  fg=TXT);

    vis.selection.bg_fill = ACC;
    vis.selection.stroke  = Stroke::new(1.0, ACC2);

    ctx.set_visuals(vis);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing    = egui::vec2(8.0, 6.0);
    style.spacing.window_margin   = Margin::same(12.0);
    style.spacing.button_padding  = egui::vec2(10.0, 5.0);
    style.spacing.indent          = 20.0;
    style.spacing.interact_size.y = 26.0;
    ctx.set_style(style);
}
