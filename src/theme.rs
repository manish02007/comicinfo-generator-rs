use eframe::egui::{self, Color32, Margin, Rounding, Shadow, Stroke};

// ── Colour palette ────────────────────────────────────────────────────────────
pub const BG:    Color32 = Color32::from_rgb(0x09, 0x09, 0x11);
pub const SURF:  Color32 = Color32::from_rgb(0x10, 0x10, 0x1e);
pub const SURF2: Color32 = Color32::from_rgb(0x18, 0x18, 0x2a);
pub const SURF3: Color32 = Color32::from_rgb(0x21, 0x21, 0x38);
pub const SURF4: Color32 = Color32::from_rgb(0x2c, 0x2c, 0x4a);
pub const ACC:   Color32 = Color32::from_rgb(0x7c, 0x6f, 0xee);
pub const ACC2:  Color32 = Color32::from_rgb(0xa8, 0x9e, 0xff);
pub const ACCD:  Color32 = Color32::from_rgb(0x4e, 0x46, 0xb4);
pub const TXT:   Color32 = Color32::from_rgb(0xea, 0xed, 0xfa);
pub const TDIM:  Color32 = Color32::from_rgb(0x8a, 0x8e, 0xb8);
pub const TMUT:  Color32 = Color32::from_rgb(0x55, 0x58, 0x7a);
pub const TGOOD: Color32 = Color32::from_rgb(0x22, 0xc5, 0x5e);
pub const TERR:  Color32 = Color32::from_rgb(0xf8, 0x71, 0x71);
pub const TWARN: Color32 = Color32::from_rgb(0xfb, 0xbf, 0x24);
pub const TINFO: Color32 = Color32::from_rgb(0x38, 0xbd, 0xe8);
pub const BDR:   Color32 = Color32::from_rgb(0x2a, 0x2a, 0x48);
pub const ROW_ALT: Color32 = Color32::from_rgb(0x14, 0x14, 0x24);

pub const BTN_PRIMARY_BG:  Color32 = ACC;
pub const BTN_SECONDARY_BG:Color32 = SURF3;
pub const BTN_DANGER_BG:   Color32 = Color32::from_rgb(0x7a, 0x20, 0x20);
pub const BTN_SUCCESS_BG:  Color32 = Color32::from_rgb(0x14, 0x6e, 0x3c);
pub const BTN_STOP_BG:     Color32 = Color32::from_rgb(0x6e, 0x18, 0x18);

// ── Pre-built frames ──────────────────────────────────────────────────────────
pub fn card() -> egui::Frame {
    egui::Frame::none()
        .fill(SURF2)
        .rounding(Rounding::same(8.0))
        .stroke(Stroke::new(1.0, BDR))
        .inner_margin(Margin::same(14.0))
}

pub fn inner_card() -> egui::Frame {
    egui::Frame::none()
        .fill(SURF3)
        .rounding(Rounding::same(6.0))
        .stroke(Stroke::new(1.0, BDR))
        .inner_margin(Margin::same(10.0))
}

pub fn log_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(BG)
        .rounding(Rounding::same(6.0))
        .inner_margin(Margin::symmetric(12.0, 8.0))
}

pub fn section_frame() -> egui::Frame { card() }
pub fn toolbar_frame() -> egui::Frame {
    egui::Frame::none()
        .fill(SURF)
        .inner_margin(Margin::symmetric(14.0, 0.0))
}

// ── Button presets ────────────────────────────────────────────────────────────
pub fn btn_primary(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(
        eframe::egui::RichText::new(label.into())
            .color(Color32::WHITE).size(12.0)
    )
    .fill(BTN_PRIMARY_BG)
    .rounding(Rounding::same(6.0))
    .min_size(egui::vec2(0.0, 28.0))
}

pub fn btn_secondary(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(
        eframe::egui::RichText::new(label.into())
            .color(TXT).size(12.0)
    )
    .fill(BTN_SECONDARY_BG)
    .stroke(Stroke::new(1.0, BDR))
    .rounding(Rounding::same(6.0))
    .min_size(egui::vec2(0.0, 28.0))
}

pub fn btn_danger(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(
        eframe::egui::RichText::new(label.into())
            .color(Color32::WHITE).size(12.0)
    )
    .fill(BTN_DANGER_BG)
    .rounding(Rounding::same(6.0))
    .min_size(egui::vec2(0.0, 28.0))
}

// ── Section header with accent left bar ──────────────────────────────────────
pub fn section_hdr(ui: &mut egui::Ui, title: &str) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let (bar, _) = ui.allocate_exact_size(
            egui::vec2(3.0, 16.0), egui::Sense::hover()
        );
        ui.painter().rect_filled(bar, Rounding::same(1.5), ACC);
        ui.add_space(7.0);
        ui.label(eframe::egui::RichText::new(title)
            .size(12.5).color(TXT).strong());
    });
    ui.add_space(8.0);
}

// Same visual style as section_hdr, plus a right-aligned "?" button.
// Sets *help_clicked to true on click, matching how ui.checkbox and
// similar egui widgets take a &mut bool to report state back to the
// caller rather than returning a Response the caller has to unpack.
pub fn section_hdr_with_help(ui: &mut egui::Ui, title: &str, help_clicked: &mut bool) {
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let (bar, _) = ui.allocate_exact_size(
            egui::vec2(3.0, 16.0), egui::Sense::hover()
        );
        ui.painter().rect_filled(bar, Rounding::same(1.5), ACC);
        ui.add_space(7.0);
        ui.label(eframe::egui::RichText::new(title)
            .size(12.5).color(TXT).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(
                egui::Button::new(eframe::egui::RichText::new("?").size(11.0).color(TDIM).strong())
                    .fill(Color32::TRANSPARENT)
                    .stroke(Stroke::new(1.0, BDR))
                    .rounding(Rounding::same(9.0))
                    .min_size(egui::vec2(18.0, 18.0))
            ).on_hover_text("What does this do?").clicked() {
                *help_clicked = true;
            }
        });
    });
    ui.add_space(8.0);
}

// ── Global style setup ────────────────────────────────────────────────────────
pub fn setup_style(ctx: &egui::Context) {
    let mut vis = egui::Visuals::dark();

    vis.override_text_color = Some(TXT);
    vis.panel_fill           = SURF;
    vis.window_fill          = SURF2;
    vis.extreme_bg_color     = BG;
    vis.faint_bg_color       = SURF2;
    vis.code_bg_color        = SURF3;
    vis.window_stroke        = Stroke::new(1.0, BDR);
    vis.window_rounding      = Rounding::same(10.0);
    vis.menu_rounding        = Rounding::same(8.0);
    vis.window_shadow        = Shadow::NONE;
    vis.popup_shadow         = Shadow::NONE;
    vis.hyperlink_color      = ACC2;
    vis.warn_fg_color        = TWARN;
    vis.error_fg_color       = TERR;

    macro_rules! wset {
        ($w:expr, fill=$f:expr, stroke=$s:expr, fg=$fg:expr) => {
            $w.bg_fill   = $f;
            $w.bg_stroke = Stroke::new(1.0, $s);
            $w.fg_stroke = Stroke::new(1.5, $fg);
            $w.rounding  = Rounding::same(5.0);
            $w.expansion = 0.0;
        };
    }
    wset!(vis.widgets.noninteractive, fill=SURF2, stroke=BDR,  fg=TDIM);
    wset!(vis.widgets.inactive,       fill=SURF3, stroke=BDR,  fg=TXT);
    wset!(vis.widgets.hovered,        fill=SURF4, stroke=ACC2, fg=TXT);
    wset!(vis.widgets.active,         fill=ACC,   stroke=ACC2, fg=Color32::WHITE);
    wset!(vis.widgets.open,           fill=SURF3, stroke=ACC,  fg=TXT);

    vis.selection.bg_fill = ACC;
    vis.selection.stroke  = Stroke::new(1.0, ACC2);

    ctx.set_visuals(vis);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing    = egui::vec2(8.0, 7.0);
    style.spacing.window_margin   = Margin::same(16.0);
    style.spacing.button_padding  = egui::vec2(12.0, 6.0);
    style.spacing.indent          = 20.0;
    style.spacing.interact_size.y = 28.0;
    ctx.set_style(style);
}