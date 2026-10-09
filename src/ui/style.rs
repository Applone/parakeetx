use iced::{Background, Border, Color, Theme, widget::{button, container, overlay::menu, pick_list, progress_bar, rule, scrollable, text, text_editor, text_input}};

use crate::settings::Appearance;

pub struct Tokens {
    pub text: Color,
    pub muted: Color,
    pub faint: Color,
    pub canvas: Color,
    pub surface: Color,
    pub line: Color,
    pub accent: Color,
    pub accent_soft: Color,
    pub record: Color,
    pub record_soft: Color,
}

const DARK: Tokens = Tokens {
    text: Color::from_rgb(0.902, 0.906, 0.898),
    muted: Color::from_rgb(0.576, 0.588, 0.580),
    faint: Color::from_rgb(0.380, 0.392, 0.388),
    canvas: Color::from_rgb(0.071, 0.075, 0.075),
    surface: Color::from_rgb(0.110, 0.118, 0.118),
    line: Color::from_rgb(0.180, 0.192, 0.192),
    accent: Color::from_rgb(0.498, 0.737, 0.627),
    accent_soft: Color::from_rgb(0.145, 0.192, 0.176),
    record: Color::from_rgb(0.839, 0.392, 0.361),
    record_soft: Color::from_rgb(0.227, 0.129, 0.125),
};

const LIGHT: Tokens = Tokens {
    text: Color::from_rgb(0.118, 0.129, 0.125),
    muted: Color::from_rgb(0.423, 0.439, 0.431),
    faint: Color::from_rgb(0.620, 0.635, 0.627),
    canvas: Color::from_rgb(0.976, 0.976, 0.969),
    surface: Color::from_rgb(1.0, 1.0, 0.996),
    line: Color::from_rgb(0.878, 0.882, 0.871),
    accent: Color::from_rgb(0.180, 0.400, 0.325),
    accent_soft: Color::from_rgb(0.898, 0.933, 0.914),
    record: Color::from_rgb(0.659, 0.251, 0.212),
    record_soft: Color::from_rgb(0.973, 0.918, 0.906),
};

pub fn tokens(theme: &Theme) -> &'static Tokens {
    if theme.extended_palette().is_dark { &DARK } else { &LIGHT }
}

pub fn theme(appearance: Appearance) -> Theme {
    let (name, tokens) = match appearance {
        Appearance::Dark => ("parakeetx dark", &DARK),
        Appearance::Light => ("parakeetx light", &LIGHT),
    };
    Theme::custom(name, iced::theme::Palette {
        background: tokens.canvas,
        text: tokens.text,
        primary: tokens.accent,
        success: tokens.accent,
        warning: tokens.record,
        danger: tokens.record,
    })
}

pub fn muted(theme: &Theme) -> text::Style {
    text::Style { color: Some(tokens(theme).muted) }
}

pub fn accent(theme: &Theme) -> text::Style {
    text::Style { color: Some(tokens(theme).accent) }
}

pub fn scrim(_: &Theme) -> container::Style {
    container::Style { background: Some(Color::from_rgba(0.02, 0.025, 0.025, 0.72).into()), ..Default::default() }
}

pub fn surface(theme: &Theme) -> container::Style {
    let tokens = tokens(theme);
    container::Style { background: Some(tokens.surface.into()), border: Border { radius: 10.0.into(), ..Default::default() }, ..Default::default() }
}

pub fn sidebar(theme: &Theme) -> container::Style {
    let tokens = tokens(theme);
    container::Style { background: Some(tokens.canvas.into()), border: Border { color: tokens.line, width: 0.0, radius: 0.0.into() }, ..Default::default() }
}

pub fn notice(theme: &Theme) -> container::Style {
    let tokens = tokens(theme);
    container::Style { background: Some(tokens.accent_soft.into()), text_color: Some(tokens.accent), border: Border { radius: 8.0.into(), ..Default::default() }, ..Default::default() }
}

pub fn alert(theme: &Theme) -> container::Style {
    let tokens = tokens(theme);
    container::Style { background: Some(tokens.record_soft.into()), text_color: Some(tokens.record), border: Border { radius: 8.0.into(), ..Default::default() }, ..Default::default() }
}

pub fn divider(theme: &Theme) -> rule::Style {
    let tokens = tokens(theme);
    rule::Style { color: tokens.line, radius: 0.0.into(), fill_mode: rule::FillMode::Full, snap: true }
}

pub fn primary(theme: &Theme, status: button::Status) -> button::Style {
    let tokens = tokens(theme);
    control(theme, tokens.accent, tokens.canvas, status)
}

pub fn record(theme: &Theme, status: button::Status) -> button::Style {
    let tokens = tokens(theme);
    control(theme, tokens.record, tokens.canvas, status)
}

pub fn secondary(theme: &Theme, status: button::Status) -> button::Style {
    let tokens = tokens(theme);
    let mut style = control(theme, Color::TRANSPARENT, tokens.text, status);
    style.border = Border { color: tokens.line, width: 1.0, radius: 6.0.into() };
    if matches!(status, button::Status::Hovered | button::Status::Pressed) {
        style.background = Some(tokens.surface.into());
    }
    style
}

pub fn quiet(theme: &Theme, status: button::Status) -> button::Style {
    let tokens = tokens(theme);
    let mut style = control(theme, Color::TRANSPARENT, tokens.muted, status);
    if matches!(status, button::Status::Hovered | button::Status::Pressed) { style.text_color = tokens.text; }
    style
}

pub fn navigation(theme: &Theme, active: bool, status: button::Status) -> button::Style {
    let tokens = tokens(theme);
    let mut style = control(theme, Color::TRANSPARENT, if active { tokens.text } else { tokens.muted }, status);
    if active {
        style.background = Some(tokens.surface.into());
    } else if status == button::Status::Hovered {
        style.text_color = tokens.text;
    }
    style
}

pub fn selectable(theme: &Theme, active: bool, status: button::Status) -> button::Style {
    let tokens = tokens(theme);
    let mut style = navigation(theme, active, status);
    if !active && status == button::Status::Hovered { style.background = Some(tokens.surface.into()); }
    style.border = Border { radius: 8.0.into(), ..Default::default() };
    style
}

fn control(theme: &Theme, background: Color, text: Color, status: button::Status) -> button::Style {
    let tokens = tokens(theme);
    let background = match status {
        button::Status::Hovered | button::Status::Pressed if background != Color::TRANSPARENT => {
            let shift = if theme.extended_palette().is_dark { 1.08 } else { 0.92 };
            Color { r: (background.r * shift).min(1.0), g: (background.g * shift).min(1.0), b: (background.b * shift).min(1.0), ..background }
        }
        button::Status::Disabled => Color { a: background.a * 0.4, ..background },
        _ => background,
    };
    button::Style {
        background: Some(Background::Color(background)),
        text_color: if status == button::Status::Disabled { tokens.faint } else { text },
        border: Border { radius: 6.0.into(), ..Default::default() },
        ..Default::default()
    }
}

pub fn field(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let tokens = tokens(theme);
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: Background::Color(tokens.surface),
        border: Border { color: if focused { tokens.accent } else { tokens.line }, width: 1.0, radius: 7.0.into() },
        icon: tokens.muted,
        placeholder: tokens.muted,
        value: tokens.text,
        selection: tokens.accent_soft,
    }
}

pub fn select(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let tokens = tokens(theme);
    pick_list::Style {
        text_color: tokens.text,
        placeholder_color: tokens.muted,
        handle_color: tokens.muted,
        background: tokens.surface.into(),
        border: Border { color: if matches!(status, pick_list::Status::Active) { tokens.line } else { tokens.accent }, width: 1.0, radius: 6.0.into() },
    }
}

pub fn menu(theme: &Theme) -> menu::Style {
    let tokens = tokens(theme);
    menu::Style {
        background: tokens.surface.into(),
        border: Border { color: tokens.line, width: 1.0, radius: 6.0.into() },
        text_color: tokens.text,
        selected_text_color: tokens.text,
        selected_background: tokens.accent_soft.into(),
        shadow: iced::Shadow::default(),
    }
}

pub fn editor(theme: &Theme, status: text_editor::Status) -> text_editor::Style {
    let tokens = tokens(theme);
    text_editor::Style {
        background: tokens.surface.into(),
        border: Border { color: if matches!(status, text_editor::Status::Focused { .. }) { tokens.accent } else { tokens.line }, width: 1.0, radius: 7.0.into() },
        placeholder: tokens.muted,
        value: tokens.text,
        selection: tokens.accent_soft,
    }
}

pub fn meter(theme: &Theme) -> progress_bar::Style {
    let tokens = tokens(theme);
    progress_bar::Style { background: tokens.line.into(), bar: tokens.accent.into(), border: Border { radius: 2.0.into(), ..Default::default() } }
}

pub fn scroller(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let tokens = tokens(theme);
    let engaged = !matches!(status, scrollable::Status::Active { .. });
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: if engaged { tokens.faint.into() } else { tokens.line.into() },
            border: Border { radius: 3.0.into(), ..Default::default() },
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: tokens.surface.into(),
            border: Border { color: tokens.line, width: 1.0, radius: 20.0.into() },
            shadow: iced::Shadow::default(),
            icon: tokens.muted,
        },
    }
}
