// The color accessor functions below (BG(), TXT(), ACC(), ...) keep the
// SCREAMING_CASE names of the `pub const`s they replace, rather than being
// renamed to snake_case, so every one of the ~115 existing call sites across
// the app only needed parens added, not a rename. This blanket allow covers
// the resulting (expected, harmless) non_snake_case warnings on those
// functions specifically -- everything else in the file still gets normal
// naming-convention lints.
#![allow(non_snake_case)]

use eframe::egui::{self, Color32, Margin, Rounding, Shadow, Stroke};
use std::sync::RwLock;
use std::time::{Duration, Instant};

// ── Theme choice (persisted in AppSettings) ──────────────────────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    MidnightViolet,
    PaperLight,
    SlateDark,
    CatppuccinMocha,
}

impl ThemeChoice {
    pub const ALL: [ThemeChoice; 4] = [
        ThemeChoice::MidnightViolet,
        ThemeChoice::PaperLight,
        ThemeChoice::SlateDark,
        ThemeChoice::CatppuccinMocha,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ThemeChoice::MidnightViolet => "Midnight Violet",
            ThemeChoice::PaperLight     => "Paper Light",
            ThemeChoice::SlateDark      => "Slate Dark",
            ThemeChoice::CatppuccinMocha => "Catppuccin Mocha",
        }
    }

    // Which egui::Visuals baseline (light() vs dark()) this theme should
    // start from. egui::Visuals::dark()/light() differ in more fields than
    // the ones this module explicitly overrides below (e.g. internal
    // widget-chrome and title-bar-adjacent values) -- starting every theme
    // from dark() left those untouched fields dark even under Paper Light,
    // which is why the window title bar and TextEdit/DragValue boxes
    // stayed dark there even though the fields we do set (panel_fill,
    // window_fill, extreme_bg_color, ...) were correctly light.
    fn is_light(self) -> bool {
        matches!(self, ThemeChoice::PaperLight)
    }

    fn palette(self) -> Palette {
        match self {
            ThemeChoice::MidnightViolet => Palette::midnight_violet(),
            ThemeChoice::PaperLight     => Palette::paper_light(),
            ThemeChoice::SlateDark      => Palette::slate_dark(),
            ThemeChoice::CatppuccinMocha => Palette::catppuccin_mocha(),
        }
    }
}

impl Default for ThemeChoice {
    fn default() -> Self { ThemeChoice::MidnightViolet }
}

// ── Palette definition ───────────────────────────────────────────────────────
// One struct instance per theme. Every color the app used to reference as a
// `pub const` now lives here instead, so it can change at runtime. Field
// names deliberately match the old constant names 1:1 (BG, SURF, TXT, ...)
// so the accessor functions below can stay equally short.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub bg:    Color32,
    pub surf:  Color32,
    pub surf2: Color32,
    pub surf3: Color32,
    pub surf4: Color32,
    pub acc:   Color32,
    pub acc2:  Color32,
    pub accd:  Color32,
    pub txt:   Color32,
    pub tdim:  Color32,
    pub tmut:  Color32,
    pub tgood: Color32,
    pub terr:  Color32,
    pub twarn: Color32,
    pub bdr:   Color32,
    pub row_alt: Color32,
    // Text color to place ON TOP of a solid `acc`-filled button. Not every
    // theme's accent has enough contrast with white (Catppuccin's mauve and
    // Slate Dark's blue both fail WCAG AA with white text), so this is
    // computed per-theme rather than hardcoded.
    pub on_accent: Color32,
    pub btn_danger_bg:  Color32,
}

impl Palette {
    fn rgb(r: u8, g: u8, b: u8) -> Color32 { Color32::from_rgb(r, g, b) }

    // The original hand-picked palette this app shipped with.
    fn midnight_violet() -> Self {
        Self {
            bg:    Self::rgb(0x09, 0x09, 0x11),
            surf:  Self::rgb(0x10, 0x10, 0x1e),
            surf2: Self::rgb(0x18, 0x18, 0x2a),
            surf3: Self::rgb(0x21, 0x21, 0x38),
            surf4: Self::rgb(0x2c, 0x2c, 0x4a),
            acc:   Self::rgb(0x7c, 0x6f, 0xee),
            acc2:  Self::rgb(0xa8, 0x9e, 0xff),
            accd:  Self::rgb(0x4e, 0x46, 0xb4),
            txt:   Self::rgb(0xea, 0xed, 0xfa),
            tdim:  Self::rgb(0x8a, 0x8e, 0xb8),
            tmut:  Self::rgb(0x55, 0x58, 0x7a),
            tgood: Self::rgb(0x22, 0xc5, 0x5e),
            terr:  Self::rgb(0xf8, 0x71, 0x71),
            twarn: Self::rgb(0xfb, 0xbf, 0x24),
            bdr:   Self::rgb(0x2a, 0x2a, 0x48),
            row_alt: Self::rgb(0x14, 0x14, 0x24),
            on_accent: Color32::WHITE,
            btn_danger_bg:  Self::rgb(0x7a, 0x20, 0x20),
        }
    }

    // A clean, neutral light theme. Kept intentionally low-saturation so it
    // reads as "paper", not as an inverted dark theme with the same accent.
    fn paper_light() -> Self {
        Self {
            bg:    Self::rgb(0xee, 0xf0, 0xf7),
            surf:  Self::rgb(0xff, 0xff, 0xff),
            surf2: Self::rgb(0xf6, 0xf7, 0xfb),
            surf3: Self::rgb(0xe8, 0xea, 0xf3),
            surf4: Self::rgb(0xd7, 0xda, 0xe9),
            acc:   Self::rgb(0x6d, 0x5c, 0xe8),
            acc2:  Self::rgb(0x57, 0x47, 0xc9),
            accd:  Self::rgb(0x8b, 0x7c, 0xf0),
            txt:   Self::rgb(0x1c, 0x1e, 0x2b),
            tdim:  Self::rgb(0x5a, 0x5d, 0x75),
            tmut:  Self::rgb(0x94, 0x98, 0xad),
            tgood: Self::rgb(0x15, 0x80, 0x3d),
            terr:  Self::rgb(0xc8, 0x27, 0x2c),
            twarn: Self::rgb(0xa8, 0x5a, 0x00),
            bdr:   Self::rgb(0xd3, 0xd6, 0xe6),
            row_alt: Self::rgb(0xe9, 0xeb, 0xf5),
            on_accent: Color32::WHITE, // WCAG AA passes here (4.83:1)
            btn_danger_bg:  Self::rgb(0xc8, 0x27, 0x2c),
        }
    }

    // A plain, neutral-gray dark theme (blue accent instead of purple) --
    // deliberately differentiated from Midnight Violet so it isn't just a
    // slight variation of the app's signature look.
    fn slate_dark() -> Self {
        Self {
            bg:    Self::rgb(0x13, 0x14, 0x17),
            surf:  Self::rgb(0x19, 0x1a, 0x1f),
            surf2: Self::rgb(0x20, 0x21, 0x27),
            surf3: Self::rgb(0x2a, 0x2b, 0x32),
            surf4: Self::rgb(0x38, 0x3a, 0x42),
            acc:   Self::rgb(0x5a, 0xa6, 0xf5),
            acc2:  Self::rgb(0x7f, 0xbc, 0xf7),
            accd:  Self::rgb(0x3a, 0x76, 0xc4),
            txt:   Self::rgb(0xe6, 0xe7, 0xeb),
            tdim:  Self::rgb(0x9a, 0x9c, 0xa6),
            tmut:  Self::rgb(0x66, 0x68, 0x6f),
            tgood: Self::rgb(0x4f, 0xce, 0x7f),
            terr:  Self::rgb(0xf0, 0x66, 0x5f),
            twarn: Self::rgb(0xe0, 0xa6, 0x3a),
            bdr:   Self::rgb(0x34, 0x36, 0x3d),
            row_alt: Self::rgb(0x1a, 0x1b, 0x20),
            on_accent: Color32::from_rgb(0x0c, 0x14, 0x1e), // white fails AA here (2.56:1)
            btn_danger_bg:  Self::rgb(0x7a, 0x20, 0x20),
        }
    }

    // Catppuccin Mocha, mapped from the project's official published hex
    // values (base/mantle/surface0-2/text/subtext0/overlay0, mauve as the
    // signature accent, lavender as the secondary accent).
    // https://catppuccin.com/palette
    fn catppuccin_mocha() -> Self {
        Self {
            bg:    Self::rgb(0x11, 0x11, 0x1b), // crust
            surf:  Self::rgb(0x18, 0x18, 0x25), // mantle
            surf2: Self::rgb(0x1e, 0x1e, 0x2e), // base
            surf3: Self::rgb(0x31, 0x32, 0x44), // surface0
            surf4: Self::rgb(0x45, 0x47, 0x5a), // surface1
            acc:   Self::rgb(0xcb, 0xa6, 0xf7), // mauve
            acc2:  Self::rgb(0xb4, 0xbe, 0xfe), // lavender
            accd:  Self::rgb(0x58, 0x5b, 0x70), // surface2
            txt:   Self::rgb(0xcd, 0xd6, 0xf4), // text
            tdim:  Self::rgb(0xa6, 0xad, 0xc8), // subtext0
            tmut:  Self::rgb(0x6c, 0x70, 0x86), // overlay0
            tgood: Self::rgb(0xa6, 0xe3, 0xa1), // green
            terr:  Self::rgb(0xf3, 0x8b, 0xa8), // red
            twarn: Self::rgb(0xf9, 0xe2, 0xaf), // yellow
            bdr:   Self::rgb(0x31, 0x32, 0x44), // surface0
            row_alt: Self::rgb(0x18, 0x18, 0x25), // mantle
            on_accent: Color32::from_rgb(0x1e, 0x1e, 0x2e), // white fails badly here (2.03:1)
            btn_danger_bg:  Self::rgb(0x8b, 0x3a, 0x4a),
        }
    }

    fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
        let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
        Color32::from_rgba_premultiplied(
            l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()), l(a.a(), b.a()),
        )
    }

    fn lerp_all(a: &Palette, b: &Palette, t: f32) -> Palette {
        Palette {
            bg:    Self::lerp(a.bg, b.bg, t),
            surf:  Self::lerp(a.surf, b.surf, t),
            surf2: Self::lerp(a.surf2, b.surf2, t),
            surf3: Self::lerp(a.surf3, b.surf3, t),
            surf4: Self::lerp(a.surf4, b.surf4, t),
            acc:   Self::lerp(a.acc, b.acc, t),
            acc2:  Self::lerp(a.acc2, b.acc2, t),
            accd:  Self::lerp(a.accd, b.accd, t),
            txt:   Self::lerp(a.txt, b.txt, t),
            tdim:  Self::lerp(a.tdim, b.tdim, t),
            tmut:  Self::lerp(a.tmut, b.tmut, t),
            tgood: Self::lerp(a.tgood, b.tgood, t),
            terr:  Self::lerp(a.terr, b.terr, t),
            twarn: Self::lerp(a.twarn, b.twarn, t),
            bdr:   Self::lerp(a.bdr, b.bdr, t),
            row_alt: Self::lerp(a.row_alt, b.row_alt, t),
            on_accent: Self::lerp(a.on_accent, b.on_accent, t),
            btn_danger_bg:  Self::lerp(a.btn_danger_bg, b.btn_danger_bg, t),
        }
    }
}

// ── Runtime theme state ──────────────────────────────────────────────────────
// A cross-fade transition in progress: interpolate from `from` to `to` over
// `duration`, starting at `started`. `current()` below returns the
// interpolated palette while a transition is active, and the plain target
// palette once it's finished -- callers never need to know which case
// they're in.
#[derive(Clone, Copy)]
struct Transition {
    from: Palette,
    to: Palette,
    started: Instant,
    duration: Duration,
}

struct ThemeState {
    choice: ThemeChoice,
    active: Palette,
    transition: Option<Transition>,
}

impl ThemeState {
    fn new() -> Self {
        let choice = ThemeChoice::default();
        Self { choice, active: choice.palette(), transition: None }
    }
}

static THEME: RwLock<Option<ThemeState>> = RwLock::new(None);

fn with_state<R>(f: impl FnOnce(&mut ThemeState) -> R) -> R {
    let mut guard = THEME.write().unwrap();
    if guard.is_none() {
        *guard = Some(ThemeState::new());
    }
    f(guard.as_mut().unwrap())
}

fn current_palette() -> Palette {
    with_state(|s| s.active)
}

/// Switches to a new theme with a fluid ~220ms cross-fade rather than an
/// instant hard cut. Call once (e.g. from the Settings theme picker); the
/// fade itself is driven by `advance_transition`, called every frame from
/// `update()`.
pub fn set_theme(choice: ThemeChoice) {
    with_state(|s| {
        if s.choice == choice { return; }
        s.transition = Some(Transition {
            from: s.active,
            to: choice.palette(),
            started: Instant::now(),
            duration: Duration::from_millis(220),
        });
        s.choice = choice;
    });
}

/// Sets the active theme with no cross-fade, for use at startup when
/// applying a saved preference -- there's no "previous" look on screen yet
/// to fade from, so animating would just delay the first correct frame.
pub fn apply_theme_immediately(choice: ThemeChoice) {
    with_state(|s| {
        s.choice = choice;
        s.active = choice.palette();
        s.transition = None;
    });
}

pub fn current_choice() -> ThemeChoice {
    with_state(|s| s.choice)
}

/// Whichever choice the active palette should be treated as "shaped like"
/// for picking a Visuals baseline -- during a transition this is already
/// `s.choice` (set immediately in set_theme, ahead of the palette actually
/// finishing its fade), so the baseline switches once, at the start of the
/// transition, rather than partway through.
fn current_baseline_is_light() -> bool {
    with_state(|s| s.choice.is_light())
}

/// Advances any in-progress theme transition and re-applies egui's global
/// style if the palette changed this frame. Must be called once per frame,
/// before any UI is drawn, so widgets rendered later in the same frame pick
/// up the interpolated colors. Requests another repaint while a transition
/// is still running so the fade actually animates instead of only updating
/// on the next unrelated repaint (e.g. mouse movement).
pub fn advance_transition(ctx: &egui::Context) {
    let (changed, still_animating) = with_state(|s| {
        let Some(t) = s.transition else { return (false, false); };
        let elapsed = t.started.elapsed().as_secs_f32();
        let dur = t.duration.as_secs_f32();
        if elapsed >= dur {
            s.active = t.to;
            s.transition = None;
            (true, false)
        } else {
            // ease-out cubic: fast start, gentle settle -- reads as
            // "fluid" rather than linear/mechanical.
            let raw = (elapsed / dur).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - raw).powi(3);
            s.active = Palette::lerp_all(&t.from, &t.to, eased);
            (true, true)
        }
    });

    if changed {
        setup_style(ctx);
    }
    if still_animating {
        ctx.request_repaint();
    }
}

// ── Colour accessors ──────────────────────────────────────────────────────────
// These replace the old `pub const NAME: Color32 = ...` values. Every call
// site that used to read `theme::TXT` now reads `theme::TXT()` -- a plain
// function call, same call-site shape, but resolved against whichever
// palette (and transition frame) is currently active. Names are kept in
// SCREAMING_CASE (rather than renamed to snake_case) so every existing call
// site only needed parens added, not a rename -- hence the blanket allow.
pub fn BG()    -> Color32 { current_palette().bg }
pub fn SURF()  -> Color32 { current_palette().surf }
pub fn SURF2() -> Color32 { current_palette().surf2 }
pub fn SURF3() -> Color32 { current_palette().surf3 }
pub fn SURF4() -> Color32 { current_palette().surf4 }
pub fn ACC()   -> Color32 { current_palette().acc }
pub fn ACC2()  -> Color32 { current_palette().acc2 }
pub fn ACCD()  -> Color32 { current_palette().accd }
pub fn TXT()   -> Color32 { current_palette().txt }
pub fn TDIM()  -> Color32 { current_palette().tdim }
pub fn TMUT()  -> Color32 { current_palette().tmut }
pub fn TGOOD() -> Color32 { current_palette().tgood }
pub fn TERR()  -> Color32 { current_palette().terr }
pub fn TWARN() -> Color32 { current_palette().twarn }
pub fn BDR()   -> Color32 { current_palette().bdr }
pub fn ROW_ALT() -> Color32 { current_palette().row_alt }
pub fn ON_ACCENT() -> Color32 { current_palette().on_accent }

pub fn BTN_PRIMARY_BG()   -> Color32 { current_palette().acc }
pub fn BTN_SECONDARY_BG() -> Color32 { current_palette().surf3 }
pub fn BTN_DANGER_BG()    -> Color32 { current_palette().btn_danger_bg }

// ── Themed window title bar ───────────────────────────────────────────────────
// egui's built-in Window title bar cannot be restyled through Visuals at
// all -- confirmed against egui's own maintainers (github.com/emilk/egui/
// discussions/2692: "For egui::Window there is no way, except to disable
// the title bar and paint it yourself"). That's why every dialog's title
// text and close button stayed illegible-dark regardless of theme even
// though the rest of each window (body, buttons, inputs) themes correctly.
//
// Fix: every egui::Window in this app calls `.title_bar(false)` and instead
// starts its content closure with `theme::window_titlebar(ui, title)` (or
// `_with_close` for the one window -- Settings -- that has a close
// button), which paints a themed title row using the same colors as
// everything else.
//
// Trade-off: egui only supports drag-to-move through its own built-in
// title bar, so disabling it also disables dragging
// (github.com/emilk/egui/issues/3619 -- egui forces the window area
// non-movable once you're painting your own content). Every window in
// this app is `.anchor(...)`-positioned (fixed, re-centered every frame)
// except Tag Order, which is a real OS-level viewport window (native title
// bar, unaffected by any of this) with an embedded-egui::Window fallback
// for backends without multi-viewport support; that rare fallback path
// loses drag-to-move too, which is an acceptable trade for a fallback that
// isn't the common case.
pub fn window_titlebar(ui: &mut egui::Ui, title: &str) {
    window_titlebar_impl(ui, title, None);
}

/// Same as `window_titlebar`, but paints a themed "x" close button on the
/// right that sets `*open = false` when clicked -- for the one window
/// (Settings) that previously used egui's built-in `.open(&mut bool)`.
pub fn window_titlebar_with_close(ui: &mut egui::Ui, title: &str, open: &mut bool) {
    window_titlebar_impl(ui, title, Some(open));
}

fn window_titlebar_impl(ui: &mut egui::Ui, title: &str, open: Option<&mut bool>) {
    egui::Frame::none()
        .fill(SURF2())
        .inner_margin(Margin::symmetric(14.0, 10.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(eframe::egui::RichText::new(title)
                    .size(16.5).color(TXT()).strong());

                if let Some(open) = open {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let resp = ui.add(
                            egui::Button::new(eframe::egui::RichText::new("X").size(12.0).color(TDIM()).strong())
                                .fill(SURF3())
                                .stroke(Stroke::new(1.0, BDR()))
                                .rounding(Rounding::same(11.0)) // circular at this size
                                .min_size(egui::vec2(22.0, 22.0))
                        );
                        if resp.clicked() {
                            *open = false;
                        }
                    });
                }
            });
        });
    // Solid accent-dark underline instead of a plain ui.separator() line --
    // gives the title bar a definite designed edge, consistent across all
    // 4 themes without needing per-theme retuning (verified against each).
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 3.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, Rounding::ZERO, ACCD());
    ui.add_space(6.0);
}

// ── Pre-built frames ──────────────────────────────────────────────────────────
pub fn card() -> egui::Frame {
    egui::Frame::none()
        .fill(SURF2())
        .rounding(Rounding::same(8.0))
        .stroke(Stroke::new(1.0, BDR()))
        .inner_margin(Margin::same(14.0))
}

// ── Button presets ────────────────────────────────────────────────────────────
pub fn btn_primary(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(
        eframe::egui::RichText::new(label.into())
            .color(ON_ACCENT()).size(12.0)
    )
    .fill(BTN_PRIMARY_BG())
    .rounding(Rounding::same(6.0))
    .min_size(egui::vec2(0.0, 28.0))
}

pub fn btn_secondary(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(
        eframe::egui::RichText::new(label.into())
            .color(TXT()).size(12.0)
    )
    .fill(BTN_SECONDARY_BG())
    .stroke(Stroke::new(1.0, BDR()))
    .rounding(Rounding::same(6.0))
    .min_size(egui::vec2(0.0, 28.0))
}

pub fn btn_danger(label: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(
        eframe::egui::RichText::new(label.into())
            .color(Color32::WHITE).size(12.0)
    )
    .fill(BTN_DANGER_BG())
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
        ui.painter().rect_filled(bar, Rounding::same(1.5), ACC());
        ui.add_space(7.0);
        ui.label(eframe::egui::RichText::new(title)
            .size(12.5).color(TXT()).strong());
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
        ui.painter().rect_filled(bar, Rounding::same(1.5), ACC());
        ui.add_space(7.0);
        ui.label(eframe::egui::RichText::new(title)
            .size(12.5).color(TXT()).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.add(
                egui::Button::new(eframe::egui::RichText::new("?").size(11.0).color(TDIM()).strong())
                    .fill(Color32::TRANSPARENT)
                    .stroke(Stroke::new(1.0, BDR()))
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
    let mut vis = if current_baseline_is_light() {
        egui::Visuals::light()
    } else {
        egui::Visuals::dark()
    };

    vis.override_text_color = Some(TXT());
    vis.panel_fill           = SURF();
    vis.window_fill          = SURF2();
    vis.extreme_bg_color     = BG();
    vis.faint_bg_color       = SURF2();
    vis.code_bg_color        = SURF3();
    vis.window_stroke        = Stroke::new(1.0, BDR());
    vis.window_rounding      = Rounding::same(10.0);
    vis.menu_rounding        = Rounding::same(8.0);
    vis.window_shadow        = Shadow::NONE;
    vis.popup_shadow         = Shadow::NONE;
    vis.hyperlink_color      = ACC2();
    vis.warn_fg_color        = TWARN();
    vis.error_fg_color       = TERR();

    macro_rules! wset {
        ($w:expr, fill=$f:expr, stroke=$s:expr, fg=$fg:expr) => {
            $w.bg_fill   = $f;
            $w.bg_stroke = Stroke::new(1.0, $s);
            $w.fg_stroke = Stroke::new(1.5, $fg);
            $w.rounding  = Rounding::same(5.0);
            $w.expansion = 0.0;
        };
    }
    wset!(vis.widgets.noninteractive, fill=SURF2(), stroke=BDR(),  fg=TDIM());
    wset!(vis.widgets.inactive,       fill=SURF3(), stroke=BDR(),  fg=TXT());
    wset!(vis.widgets.hovered,        fill=SURF4(), stroke=ACC2(), fg=TXT());
    wset!(vis.widgets.active,         fill=ACC(),   stroke=ACC2(), fg=ON_ACCENT());
    wset!(vis.widgets.open,           fill=SURF3(), stroke=ACC(),  fg=TXT());

    vis.selection.bg_fill = ACC();
    vis.selection.stroke  = Stroke::new(1.0, ACC2());

    ctx.set_visuals(vis);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing    = egui::vec2(8.0, 7.0);
    style.spacing.window_margin   = Margin::same(16.0);
    style.spacing.button_padding  = egui::vec2(12.0, 6.0);
    style.spacing.indent          = 20.0;
    style.spacing.interact_size.y = 28.0;
    ctx.set_style(style);
}