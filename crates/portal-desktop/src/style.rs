//! How the window looks: dark, with white buttons for the main action of
//! each screen and quiet outlined ones for everything else.

use iced::theme::Palette;
use iced::widget::{button, container, progress_bar, text, text_input};
use iced::{Background, Border, Color, Shadow, Theme, color};

/// The window behind everything.
const BACKGROUND: Color = color!(0x121315);
/// Cards and inputs.
const SURFACE: Color = color!(0x1b1c1f);
/// Hovered controls and the empty part of progress bars.
const RAISED: Color = color!(0x27282c);
/// Borders.
const LINE: Color = color!(0x303136);
const TEXT: Color = color!(0xededee);
const MUTED: Color = color!(0x9b9ca3);
const WHITE: Color = color!(0xffffff);
/// Text on white buttons.
const INK: Color = color!(0x111214);
const SUCCESS: Color = color!(0x5fd38d);
const DANGER: Color = color!(0xf07a7a);

const RADIUS: f32 = 10.0;

/// The theme, whose primary color (used by checkboxes, focused inputs and
/// selections) is white too.
pub fn theme() -> Theme {
    Theme::custom(
        "Portal",
        Palette {
            background: BACKGROUND,
            text: TEXT,
            primary: WHITE,
            success: SUCCESS,
            warning: color!(0xe8b45a),
            danger: DANGER,
        },
    )
}

fn border(color: Color, width: f32, radius: f32) -> Border {
    Border {
        color,
        width,
        radius: radius.into(),
    }
}

fn button_style(background: Option<Color>, text_color: Color, border: Border) -> button::Style {
    button::Style {
        background: background.map(Background::Color),
        text_color,
        border,
        shadow: Shadow::default(),
        snap: true,
    }
}

/// The main action: white with dark text.
pub fn primary(_: &Theme, status: button::Status) -> button::Style {
    let (background, text_color) = match status {
        button::Status::Active => (WHITE, INK),
        button::Status::Hovered => (color!(0xe4e4e6), INK),
        button::Status::Pressed => (color!(0xcfcfd2), INK),
        button::Status::Disabled => (WHITE.scale_alpha(0.18), TEXT.scale_alpha(0.45)),
    };
    button_style(
        Some(background),
        text_color,
        border(Color::TRANSPARENT, 0.0, RADIUS),
    )
}

/// Other actions: outlined.
pub fn secondary(_: &Theme, status: button::Status) -> button::Style {
    let (background, text_color, line) = match status {
        button::Status::Active => (None, TEXT, LINE),
        button::Status::Hovered => (Some(RAISED), WHITE, color!(0x45464c)),
        button::Status::Pressed => (Some(LINE), WHITE, color!(0x45464c)),
        button::Status::Disabled => (None, MUTED.scale_alpha(0.5), LINE.scale_alpha(0.5)),
    };
    button_style(background, text_color, border(line, 1.0, RADIUS))
}

/// Small actions inside lists, like "Remove": just text.
pub fn quiet(_: &Theme, status: button::Status) -> button::Style {
    let text_color = match status {
        button::Status::Hovered | button::Status::Pressed => TEXT,
        _ => MUTED,
    };
    button_style(None, text_color, border(Color::TRANSPARENT, 0.0, RADIUS))
}

/// One of the Send / Receive switches at the top.
pub fn tab(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let (background, text_color) = match (active, status) {
            (true, _) => (Some(WHITE), INK),
            (false, button::Status::Hovered | button::Status::Pressed) => (Some(RAISED), TEXT),
            (false, _) => (None, MUTED),
        };
        button_style(background, text_color, border(Color::TRANSPARENT, 0.0, 8.0))
    }
}

/// The pill holding the tabs.
pub fn tabs(_: &Theme) -> container::Style {
    container::Style {
        background: Some(SURFACE.into()),
        border: border(LINE, 1.0, RADIUS + 1.0),
        ..container::Style::default()
    }
}

/// A raised panel.
pub fn card(_: &Theme) -> container::Style {
    container::Style {
        background: Some(SURFACE.into()),
        border: border(LINE, 1.0, 16.0),
        ..container::Style::default()
    }
}

/// Where files can be dropped; lights up while they are dragged over it.
pub fn drop_zone(hovering: bool) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(if hovering { RAISED } else { SURFACE }.into()),
        border: border(if hovering { WHITE } else { LINE }, 1.5, 16.0),
        ..container::Style::default()
    }
}

/// The round badge on a finished screen.
pub fn badge(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(color.scale_alpha(0.14).into()),
        text_color: Some(color),
        border: border(color.scale_alpha(0.35), 1.0, 28.0),
        ..container::Style::default()
    }
}

pub fn success_color() -> Color {
    SUCCESS
}

pub fn danger_color() -> Color {
    DANGER
}

pub fn input(_: &Theme, status: text_input::Status) -> text_input::Style {
    let line = match status {
        text_input::Status::Focused { .. } => WHITE.scale_alpha(0.85),
        text_input::Status::Hovered => color!(0x45464c),
        _ => LINE,
    };
    text_input::Style {
        background: SURFACE.into(),
        border: border(line, 1.0, RADIUS),
        icon: MUTED,
        placeholder: color!(0x5d5e64),
        value: WHITE,
        selection: WHITE.scale_alpha(0.25),
    }
}

pub fn bar(_: &Theme) -> progress_bar::Style {
    progress_bar::Style {
        background: RAISED.into(),
        bar: WHITE.into(),
        border: border(Color::TRANSPARENT, 0.0, 99.0),
    }
}

/// Secondary text: hints, labels, details.
pub fn muted(_: &Theme) -> text::Style {
    text::Style { color: Some(MUTED) }
}

pub fn danger(_: &Theme) -> text::Style {
    text::Style {
        color: Some(DANGER),
    }
}
