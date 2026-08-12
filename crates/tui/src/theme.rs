//! Semantic theme tokens (STEP 1.12 RULE 7).
//!
//! Widgets must never hard-code colors — every color they draw is read from a
//! [`Theme`] token. That keeps the palette swappable (dark, high-contrast,
//! color-blind-safe variants can be added later without touching a single
//! widget) and matches the [Chapter 10](../../docs/docs/10-ide-github-and-inputs.md)
//! `Theme` shape (`surface / text / status / syntax / diff / agent`), extended
//! here with explicit `focus` and `selection` groups the layout needs.

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};

/// Backgrounds and structural chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SurfaceTokens {
    /// The overall terminal background.
    pub background: Color,
    /// A raised panel / pane body.
    pub panel: Color,
    /// A pane border when the pane is not focused.
    pub border: Color,
    /// The background of an overlay / modal.
    pub overlay: Color,
    /// The background of the user's own message container (the `You` turn). A
    /// subtly-raised surface distinct from `panel`; == `panel` on depths with no
    /// distinct subtle surface (ansi16/monochrome), which fall back to an accent bar.
    pub user: Color,
}

/// Foreground text roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextTokens {
    /// Body text.
    pub primary: Color,
    /// Supporting / dimmer text.
    pub secondary: Color,
    /// De-emphasized text (timestamps, hints).
    pub muted: Color,
    /// Section headings / titles.
    pub heading: Color,
}

/// Status roles used by the status line, run state, and notices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusTokens {
    pub info: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
    /// An actively running / working state.
    pub running: Color,
    /// An idle / terminal-but-fine state.
    pub idle: Color,
}

/// Syntax roles (used when rendering code / commands in tool cards).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxTokens {
    pub keyword: Color,
    pub literal: Color,
    pub string: Color,
    pub comment: Color,
    /// Type / struct / class / namespace names.
    pub r#type: Color,
    /// Function / method / macro names.
    pub function: Color,
    /// Operators (`+`, `=>`, `::`).
    pub operator: Color,
    /// Named constants / booleans / references.
    pub constant: Color,
    /// Brackets and separators.
    pub punctuation: Color,
}

/// Diff / patch roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffTokens {
    pub added: Color,
    pub removed: Color,
    pub context: Color,
    pub header: Color,
}

/// Agent-activity roles (model text, tool cards, thinking).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentTokens {
    /// Streamed model prose.
    pub model_text: Color,
    /// A tool card accent.
    pub tool: Color,
    /// Thinking / internal reasoning markers.
    pub thinking: Color,
}

/// Focus indication (which pane the keyboard drives).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FocusTokens {
    /// The border/accent of the focused pane.
    pub active: Color,
    /// The border/accent of an unfocused pane.
    pub inactive: Color,
}

/// Selection highlight (selected list row / transcript entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionTokens {
    pub foreground: Color,
    pub background: Color,
}

/// A complete set of semantic tokens. Constructed once and threaded through
/// every render call; widgets only ever read from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub surface: SurfaceTokens,
    pub text: TextTokens,
    pub status: StatusTokens,
    pub syntax: SyntaxTokens,
    pub diff: DiffTokens,
    pub agent: AgentTokens,
    pub focus: FocusTokens,
    pub selection: SelectionTokens,
}

impl Theme {
    /// The built-in dark theme (STEP 1.12 RULE 7: ship at least a dark theme).
    #[must_use]
    pub const fn dark() -> Self {
        Self {
            surface: SurfaceTokens {
                background: Color::Rgb(0x0b, 0x0d, 0x12),
                panel: Color::Rgb(0x11, 0x14, 0x1b),
                border: Color::Rgb(0x2a, 0x2f, 0x3a),
                overlay: Color::Rgb(0x17, 0x1b, 0x25),
                user: Color::Rgb(0x18, 0x16, 0x24),
            },
            text: TextTokens {
                primary: Color::Rgb(0xe8, 0xec, 0xf4),
                secondary: Color::Rgb(0xb2, 0xb9, 0xc8),
                muted: Color::Rgb(0x85, 0x8d, 0x9d),
                heading: Color::Rgb(0xf8, 0xfa, 0xfc),
            },
            status: StatusTokens {
                info: Color::Rgb(0x60, 0xa5, 0xfa),
                success: Color::Rgb(0x34, 0xd3, 0x99),
                warning: Color::Rgb(0xfb, 0xbf, 0x24),
                error: Color::Rgb(0xfb, 0x71, 0x85),
                running: Color::Rgb(0x67, 0xe8, 0xf9),
                idle: Color::Rgb(0x94, 0x9b, 0xaa),
            },
            syntax: SyntaxTokens {
                keyword: Color::Rgb(0xc6, 0x92, 0xff),
                literal: Color::Rgb(0xe6, 0xb4, 0x50),
                string: Color::Rgb(0x9c, 0xd6, 0x7a),
                comment: Color::Rgb(0x85, 0x8d, 0x9d),
                r#type: Color::Rgb(0x5c, 0xc2, 0xc0),
                function: Color::Rgb(0x6c, 0xb0, 0xf0),
                operator: Color::Rgb(0xc3, 0xca, 0xd6),
                constant: Color::Rgb(0xe8, 0x8a, 0x6a),
                punctuation: Color::Rgb(0x9a, 0xa2, 0xb1),
            },
            diff: DiffTokens {
                added: Color::Rgb(0x5d, 0xd6, 0x9a),
                removed: Color::Rgb(0xef, 0x6d, 0x6d),
                context: Color::Rgb(0x9a, 0xa2, 0xb1),
                header: Color::Rgb(0x5c, 0x9d, 0xff),
            },
            agent: AgentTokens {
                model_text: Color::Rgb(0xdc, 0xe2, 0xec),
                tool: Color::Rgb(0x67, 0xe8, 0xf9),
                thinking: Color::Rgb(0x94, 0x9b, 0xaa),
            },
            focus: FocusTokens {
                active: Color::Rgb(0xa7, 0x8b, 0xfa),
                inactive: Color::Rgb(0x2a, 0x2f, 0x3a),
            },
            selection: SelectionTokens {
                foreground: Color::Rgb(0xf8, 0xfa, 0xfc),
                background: Color::Rgb(0x30, 0x29, 0x4a),
            },
        }
    }

    /// A true-color **light** variant, for light terminals. The same semantic
    /// tokens, inverted for a bright background.
    #[must_use]
    pub const fn light() -> Self {
        Self {
            surface: SurfaceTokens {
                background: Color::Rgb(0xfa, 0xfb, 0xfc),
                panel: Color::Rgb(0xff, 0xff, 0xff),
                border: Color::Rgb(0xd0, 0xd7, 0xde),
                overlay: Color::Rgb(0xf0, 0xf2, 0xf5),
                user: Color::Rgb(0xea, 0xec, 0xf1),
            },
            text: TextTokens {
                primary: Color::Rgb(0x1f, 0x23, 0x28),
                secondary: Color::Rgb(0x4a, 0x52, 0x5e),
                muted: Color::Rgb(0x62, 0x6b, 0x78),
                heading: Color::Rgb(0x0d, 0x11, 0x17),
            },
            status: StatusTokens {
                info: Color::Rgb(0x0a, 0x5a, 0xd0),
                success: Color::Rgb(0x1a, 0x7f, 0x4b),
                warning: Color::Rgb(0x9a, 0x6b, 0x00),
                error: Color::Rgb(0xc0, 0x2a, 0x2a),
                running: Color::Rgb(0x0a, 0x6a, 0xa8),
                idle: Color::Rgb(0x6b, 0x73, 0x82),
            },
            syntax: SyntaxTokens {
                keyword: Color::Rgb(0x7a, 0x30, 0xc0),
                literal: Color::Rgb(0x9a, 0x6b, 0x00),
                string: Color::Rgb(0x1a, 0x7f, 0x4b),
                comment: Color::Rgb(0x62, 0x6b, 0x78),
                r#type: Color::Rgb(0x0a, 0x7a, 0x78),
                function: Color::Rgb(0x08, 0x4a, 0xc0),
                operator: Color::Rgb(0x4a, 0x52, 0x5e),
                constant: Color::Rgb(0xb0, 0x4a, 0x00),
                punctuation: Color::Rgb(0x6b, 0x73, 0x82),
            },
            diff: DiffTokens {
                added: Color::Rgb(0x1a, 0x7f, 0x4b),
                removed: Color::Rgb(0xc0, 0x2a, 0x2a),
                context: Color::Rgb(0x4a, 0x52, 0x5e),
                header: Color::Rgb(0x0a, 0x5a, 0xd0),
            },
            agent: AgentTokens {
                model_text: Color::Rgb(0x1f, 0x23, 0x28),
                tool: Color::Rgb(0x0a, 0x6a, 0xa8),
                thinking: Color::Rgb(0x6b, 0x73, 0x82),
            },
            focus: FocusTokens {
                active: Color::Rgb(0x0a, 0x5a, 0xd0),
                inactive: Color::Rgb(0xd0, 0xd7, 0xde),
            },
            selection: SelectionTokens {
                foreground: Color::Rgb(0x0d, 0x11, 0x17),
                background: Color::Rgb(0xdd, 0xe9, 0xfb),
            },
        }
    }

    /// A **high-contrast** variant: pure black background, pure white text, and
    /// maximally saturated status colors — the accessibility baseline for low
    /// vision. Every token is deliberately far from every other in luminance.
    #[must_use]
    pub const fn high_contrast() -> Self {
        Self {
            surface: SurfaceTokens {
                background: Color::Rgb(0x00, 0x00, 0x00),
                panel: Color::Rgb(0x00, 0x00, 0x00),
                border: Color::Rgb(0xff, 0xff, 0xff),
                overlay: Color::Rgb(0x0a, 0x0a, 0x0a),
                user: Color::Rgb(0x1a, 0x1a, 0x1a),
            },
            text: TextTokens {
                primary: Color::Rgb(0xff, 0xff, 0xff),
                secondary: Color::Rgb(0xe0, 0xe0, 0xe0),
                muted: Color::Rgb(0xc0, 0xc0, 0xc0),
                heading: Color::Rgb(0xff, 0xff, 0xff),
            },
            status: StatusTokens {
                info: Color::Rgb(0x00, 0xd7, 0xff),
                success: Color::Rgb(0x00, 0xff, 0x5f),
                warning: Color::Rgb(0xff, 0xd7, 0x00),
                error: Color::Rgb(0xff, 0x30, 0x30),
                running: Color::Rgb(0x00, 0xd7, 0xff),
                idle: Color::Rgb(0xc0, 0xc0, 0xc0),
            },
            syntax: SyntaxTokens {
                keyword: Color::Rgb(0xff, 0x80, 0xff),
                literal: Color::Rgb(0xff, 0xd7, 0x00),
                string: Color::Rgb(0x00, 0xff, 0x5f),
                comment: Color::Rgb(0xc0, 0xc0, 0xc0),
                r#type: Color::Rgb(0x00, 0xff, 0xd7),
                function: Color::Rgb(0x00, 0xd7, 0xff),
                operator: Color::Rgb(0xff, 0xff, 0xff),
                constant: Color::Rgb(0xff, 0xa5, 0x00),
                punctuation: Color::Rgb(0xe0, 0xe0, 0xe0),
            },
            diff: DiffTokens {
                added: Color::Rgb(0x00, 0xff, 0x5f),
                removed: Color::Rgb(0xff, 0x30, 0x30),
                context: Color::Rgb(0xe0, 0xe0, 0xe0),
                header: Color::Rgb(0x00, 0xd7, 0xff),
            },
            agent: AgentTokens {
                model_text: Color::Rgb(0xff, 0xff, 0xff),
                tool: Color::Rgb(0x00, 0xd7, 0xff),
                thinking: Color::Rgb(0xc0, 0xc0, 0xc0),
            },
            focus: FocusTokens {
                active: Color::Rgb(0xff, 0xff, 0x00),
                inactive: Color::Rgb(0x80, 0x80, 0x80),
            },
            selection: SelectionTokens {
                foreground: Color::Rgb(0x00, 0x00, 0x00),
                background: Color::Rgb(0xff, 0xff, 0x00),
            },
        }
    }

    /// A **color-blind-safe** variant using the Okabe–Ito palette — hues chosen
    /// to stay distinct under deuteranopia/protanopia/tritanopia. Notably it
    /// avoids the red/green pairing for added/removed, using vermillion vs.
    /// bluish-green (distinguishable) rather than pure red vs. green.
    #[must_use]
    pub const fn color_blind_safe() -> Self {
        // Okabe–Ito: orange E69F00, sky-blue 56B4E9, bluish-green 009E73,
        // yellow F0E442, blue 0072B2, vermillion D55E00, reddish-purple CC79A7.
        Self {
            surface: SurfaceTokens {
                background: Color::Rgb(0x11, 0x13, 0x17),
                panel: Color::Rgb(0x1b, 0x1e, 0x24),
                border: Color::Rgb(0x3a, 0x40, 0x4a),
                overlay: Color::Rgb(0x23, 0x27, 0x2f),
                user: Color::Rgb(0x23, 0x27, 0x2f),
            },
            text: TextTokens {
                primary: Color::Rgb(0xed, 0xf0, 0xf5),
                secondary: Color::Rgb(0xbc, 0xc2, 0xce),
                muted: Color::Rgb(0x86, 0x8e, 0x9d),
                heading: Color::Rgb(0xf5, 0xf7, 0xfb),
            },
            status: StatusTokens {
                info: Color::Rgb(0x56, 0xb4, 0xe9),    // sky blue
                success: Color::Rgb(0x00, 0x9e, 0x73), // bluish green
                warning: Color::Rgb(0xe6, 0x9f, 0x00), // orange
                error: Color::Rgb(0xd5, 0x5e, 0x00),   // vermillion
                running: Color::Rgb(0x56, 0xb4, 0xe9),
                idle: Color::Rgb(0x94, 0x9c, 0xac),
            },
            syntax: SyntaxTokens {
                keyword: Color::Rgb(0xcc, 0x79, 0xa7), // reddish purple
                literal: Color::Rgb(0xe6, 0x9f, 0x00),
                string: Color::Rgb(0x00, 0x9e, 0x73),
                comment: Color::Rgb(0x86, 0x8e, 0x9d),
                r#type: Color::Rgb(0x56, 0xb4, 0xe9),
                function: Color::Rgb(0x00, 0x72, 0xb2),
                operator: Color::Rgb(0xbc, 0xc2, 0xce),
                constant: Color::Rgb(0xd5, 0x5e, 0x00),
                punctuation: Color::Rgb(0x94, 0x9c, 0xac),
            },
            diff: DiffTokens {
                added: Color::Rgb(0x00, 0x9e, 0x73), // bluish green (not pure green)
                removed: Color::Rgb(0xd5, 0x5e, 0x00), // vermillion (not pure red)
                context: Color::Rgb(0xbc, 0xc2, 0xce),
                header: Color::Rgb(0x56, 0xb4, 0xe9),
            },
            agent: AgentTokens {
                model_text: Color::Rgb(0xed, 0xf0, 0xf5),
                tool: Color::Rgb(0x56, 0xb4, 0xe9),
                thinking: Color::Rgb(0x94, 0x9c, 0xac),
            },
            focus: FocusTokens {
                active: Color::Rgb(0x56, 0xb4, 0xe9),
                inactive: Color::Rgb(0x3a, 0x40, 0x4a),
            },
            selection: SelectionTokens {
                foreground: Color::Rgb(0xf8, 0xfa, 0xfc),
                background: Color::Rgb(0x20, 0x3c, 0x4f),
            },
        }
    }

    /// A **256-color** variant built from the xterm-256 indexed palette, for
    /// terminals without 24-bit color. Uses `Color::Indexed` throughout.
    #[must_use]
    pub const fn ansi256() -> Self {
        Self {
            surface: SurfaceTokens {
                background: Color::Indexed(234),
                panel: Color::Indexed(235),
                border: Color::Indexed(240),
                overlay: Color::Indexed(237),
                user: Color::Indexed(236),
            },
            text: TextTokens {
                primary: Color::Indexed(253),
                secondary: Color::Indexed(250),
                muted: Color::Indexed(248),
                heading: Color::Indexed(255),
            },
            status: StatusTokens {
                info: Color::Indexed(75),
                success: Color::Indexed(78),
                warning: Color::Indexed(179),
                error: Color::Indexed(203),
                running: Color::Indexed(81),
                idle: Color::Indexed(245),
            },
            syntax: SyntaxTokens {
                keyword: Color::Indexed(141),
                literal: Color::Indexed(179),
                string: Color::Indexed(114),
                comment: Color::Indexed(248),
                r#type: Color::Indexed(80),
                function: Color::Indexed(75),
                operator: Color::Indexed(249),
                constant: Color::Indexed(173),
                punctuation: Color::Indexed(245),
            },
            diff: DiffTokens {
                added: Color::Indexed(78),
                removed: Color::Indexed(203),
                context: Color::Indexed(249),
                header: Color::Indexed(75),
            },
            agent: AgentTokens {
                model_text: Color::Indexed(252),
                tool: Color::Indexed(81),
                thinking: Color::Indexed(245),
            },
            focus: FocusTokens {
                active: Color::Indexed(75),
                inactive: Color::Indexed(240),
            },
            selection: SelectionTokens {
                foreground: Color::Indexed(255),
                background: Color::Indexed(60),
            },
        }
    }

    /// A **16-color** variant using only the basic ANSI palette, so every widget
    /// stays legible on a 16-color terminal (STEP 6.6 fallback). Bright variants
    /// separate accents from body text.
    #[must_use]
    pub const fn ansi16() -> Self {
        Self {
            surface: SurfaceTokens {
                background: Color::Black,
                panel: Color::Black,
                border: Color::DarkGray,
                overlay: Color::Black,
                user: Color::Black,
            },
            text: TextTokens {
                primary: Color::White,
                secondary: Color::Gray,
                muted: Color::DarkGray,
                heading: Color::White,
            },
            status: StatusTokens {
                info: Color::LightBlue,
                success: Color::LightGreen,
                warning: Color::LightYellow,
                error: Color::LightRed,
                running: Color::LightCyan,
                idle: Color::Gray,
            },
            syntax: SyntaxTokens {
                keyword: Color::LightMagenta,
                literal: Color::LightYellow,
                string: Color::LightGreen,
                comment: Color::DarkGray,
                r#type: Color::LightCyan,
                function: Color::LightBlue,
                operator: Color::Gray,
                constant: Color::Yellow,
                punctuation: Color::Gray,
            },
            diff: DiffTokens {
                added: Color::LightGreen,
                removed: Color::LightRed,
                context: Color::Gray,
                header: Color::LightBlue,
            },
            agent: AgentTokens {
                model_text: Color::White,
                tool: Color::LightCyan,
                thinking: Color::Gray,
            },
            focus: FocusTokens {
                active: Color::LightCyan,
                inactive: Color::DarkGray,
            },
            selection: SelectionTokens {
                foreground: Color::Black,
                background: Color::Gray,
            },
        }
    }

    /// A **monochrome** variant: no color at all, only white/gray/black. Widgets
    /// stay legible on a monochrome terminal; distinction comes from luminance and
    /// the text modifiers the render layer applies (bold selection, etc.).
    #[must_use]
    pub const fn monochrome() -> Self {
        Self {
            surface: SurfaceTokens {
                background: Color::Black,
                panel: Color::Black,
                border: Color::Gray,
                overlay: Color::Black,
                user: Color::Black,
            },
            text: TextTokens {
                primary: Color::White,
                secondary: Color::Gray,
                muted: Color::DarkGray,
                heading: Color::White,
            },
            status: StatusTokens {
                info: Color::White,
                success: Color::White,
                warning: Color::Gray,
                error: Color::White,
                running: Color::White,
                idle: Color::DarkGray,
            },
            syntax: SyntaxTokens {
                keyword: Color::White,
                literal: Color::Gray,
                string: Color::Gray,
                comment: Color::DarkGray,
                r#type: Color::Gray,
                function: Color::Gray,
                operator: Color::DarkGray,
                constant: Color::Gray,
                punctuation: Color::DarkGray,
            },
            diff: DiffTokens {
                added: Color::White,
                removed: Color::Gray,
                context: Color::DarkGray,
                header: Color::White,
            },
            agent: AgentTokens {
                model_text: Color::White,
                tool: Color::Gray,
                thinking: Color::DarkGray,
            },
            focus: FocusTokens {
                active: Color::White,
                inactive: Color::DarkGray,
            },
            selection: SelectionTokens {
                foreground: Color::Black,
                background: Color::White,
            },
        }
    }

    /// Construct the theme for a named [`ThemeVariant`].
    #[must_use]
    pub const fn variant(v: ThemeVariant) -> Self {
        match v {
            ThemeVariant::Dark => Self::dark(),
            ThemeVariant::Light => Self::light(),
            ThemeVariant::HighContrast => Self::high_contrast(),
            ThemeVariant::ColorBlindSafe => Self::color_blind_safe(),
            ThemeVariant::Ansi256 => Self::ansi256(),
            ThemeVariant::Ansi16 => Self::ansi16(),
            ThemeVariant::Monochrome => Self::monochrome(),
        }
    }

    /// Pick the best theme for a terminal's detected [`ColorDepth`] and the user's
    /// [`ThemePreferences`]. A manual override always wins (STEP 6.6: "capability
    /// detection picks the best variant with manual override"); otherwise
    /// accessibility preferences take precedence over depth, then depth chooses
    /// the fidelity.
    #[must_use]
    pub fn select(depth: ColorDepth, prefs: ThemePreferences) -> Self {
        if let Some(override_variant) = prefs.override_variant {
            return Self::variant(override_variant);
        }
        // Accessibility needs come before aesthetics — but only where the terminal
        // can render the distinct colors they rely on.
        if depth == ColorDepth::Monochrome {
            return Self::monochrome();
        }
        if prefs.high_contrast {
            return Self::high_contrast();
        }
        match depth {
            ColorDepth::TrueColor => {
                if prefs.color_blind_safe {
                    Self::color_blind_safe()
                } else if prefs.prefer_light {
                    Self::light()
                } else {
                    Self::dark()
                }
            }
            ColorDepth::Ansi256 => Self::ansi256(),
            ColorDepth::Ansi16 => Self::ansi16(),
            ColorDepth::Monochrome => Self::monochrome(),
        }
    }

    /// Base style for pane bodies (panel background, primary text).
    #[must_use]
    pub fn panel_style(&self) -> Style {
        Style::default()
            .bg(self.surface.panel)
            .fg(self.text.primary)
    }

    /// Border color for a pane, depending on whether it is focused.
    #[must_use]
    pub fn border_color(&self, focused: bool) -> Color {
        if focused {
            self.focus.active
        } else {
            self.focus.inactive
        }
    }

    /// Highlight style for the selected row / entry.
    #[must_use]
    pub fn selection_style(&self) -> Style {
        Style::default()
            .fg(self.selection.foreground)
            .bg(self.selection.background)
            .add_modifier(Modifier::BOLD)
    }

    /// Foreground style for content inside a selectable row. Ratatui merges a
    /// `ListItem` style with each child span, and an explicit child foreground
    /// wins over the parent. Every selected child therefore has to opt into the
    /// selection foreground or muted/status text can become indistinguishable
    /// from a tonal selection background (the original ANSI16 mapping exposed
    /// this as DarkGray-on-DarkGray).
    #[must_use]
    pub fn selection_aware_text_style(&self, selected: bool, foreground: Color) -> Style {
        let style = Style::default().fg(if selected {
            self.selection.foreground
        } else {
            foreground
        });
        if selected {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

/// The color fidelity a terminal supports, detected from the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    /// 24-bit direct color (`COLORTERM=truecolor`).
    TrueColor,
    /// 256 indexed colors (`TERM=*-256color`).
    Ansi256,
    /// The 16 basic ANSI colors.
    Ansi16,
    /// No color (`NO_COLOR` set, or `TERM=dumb`).
    Monochrome,
}

impl ColorDepth {
    /// Detect the terminal's color depth from environment variables, following
    /// the de-facto conventions: `NO_COLOR` disables color entirely; `COLORTERM`
    /// of `truecolor`/`24bit` means direct color; a `256color` `TERM` means 256;
    /// a `dumb`/empty `TERM` means monochrome; otherwise assume 16.
    #[must_use]
    pub fn detect() -> Self {
        Self::from_env(
            std::env::var("NO_COLOR").ok().as_deref(),
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }

    /// The pure detection rule, over explicit values (so it is testable without
    /// mutating the process environment).
    #[must_use]
    pub fn from_env(no_color: Option<&str>, colorterm: Option<&str>, term: Option<&str>) -> Self {
        // NO_COLOR (any non-empty value) forces monochrome — the user opted out.
        if no_color.is_some_and(|v| !v.is_empty()) {
            return ColorDepth::Monochrome;
        }
        if let Some(ct) = colorterm {
            if ct.eq_ignore_ascii_case("truecolor") || ct.eq_ignore_ascii_case("24bit") {
                return ColorDepth::TrueColor;
            }
        }
        match term {
            None => ColorDepth::Ansi16,
            Some(t) if t.is_empty() || t == "dumb" => ColorDepth::Monochrome,
            Some(t) if t.contains("256color") => ColorDepth::Ansi256,
            Some(t) if t.contains("truecolor") || t.contains("direct") => ColorDepth::TrueColor,
            Some(_) => ColorDepth::Ansi16,
        }
    }
}

/// A named built-in theme variant. STEP 6.6 ships six: true-color dark, light,
/// high-contrast, color-blind-safe, 256-color, 16-color, and monochrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeVariant {
    Dark,
    Light,
    HighContrast,
    ColorBlindSafe,
    Ansi256,
    Ansi16,
    Monochrome,
}

/// User theme preferences layered over terminal detection. A manual
/// `override_variant` wins outright; otherwise accessibility flags steer the
/// choice within what the terminal can render.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThemePreferences {
    /// Prefer the high-contrast variant (low-vision accessibility).
    pub high_contrast: bool,
    /// Prefer the color-blind-safe (Okabe–Ito) palette.
    pub color_blind_safe: bool,
    /// Prefer the light variant on a true-color terminal.
    pub prefer_light: bool,
    /// An explicit manual override — always honored.
    pub override_variant: Option<ThemeVariant>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color_rgb(color: Color) -> Option<(u8, u8, u8)> {
        let named = match color {
            Color::Reset => return None,
            Color::Black => (0, 0, 0),
            Color::Red => (128, 0, 0),
            Color::Green => (0, 128, 0),
            Color::Yellow => (128, 128, 0),
            Color::Blue => (0, 0, 128),
            Color::Magenta => (128, 0, 128),
            Color::Cyan => (0, 128, 128),
            Color::Gray => (192, 192, 192),
            Color::DarkGray => (128, 128, 128),
            Color::LightRed => (255, 0, 0),
            Color::LightGreen => (0, 255, 0),
            Color::LightYellow => (255, 255, 0),
            Color::LightBlue => (0, 0, 255),
            Color::LightMagenta => (255, 0, 255),
            Color::LightCyan => (0, 255, 255),
            Color::White => (255, 255, 255),
            Color::Rgb(r, g, b) => (r, g, b),
            Color::Indexed(index) if index < 16 => {
                const ANSI: [(u8, u8, u8); 16] = [
                    (0, 0, 0),
                    (128, 0, 0),
                    (0, 128, 0),
                    (128, 128, 0),
                    (0, 0, 128),
                    (128, 0, 128),
                    (0, 128, 128),
                    (192, 192, 192),
                    (128, 128, 128),
                    (255, 0, 0),
                    (0, 255, 0),
                    (255, 255, 0),
                    (0, 0, 255),
                    (255, 0, 255),
                    (0, 255, 255),
                    (255, 255, 255),
                ];
                ANSI[usize::from(index)]
            }
            Color::Indexed(index) if index < 232 => {
                const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
                let offset = index - 16;
                (
                    CUBE[usize::from(offset / 36)],
                    CUBE[usize::from((offset % 36) / 6)],
                    CUBE[usize::from(offset % 6)],
                )
            }
            Color::Indexed(index) => {
                let gray = 8_u8.saturating_add((index - 232).saturating_mul(10));
                (gray, gray, gray)
            }
        };
        Some(named)
    }

    fn relative_luminance(color: Color) -> f64 {
        let (r, g, b) = color_rgb(color).expect("theme colors are explicit");
        let linear = |channel: u8| {
            let value = f64::from(channel) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    fn contrast_ratio(foreground: Color, background: Color) -> f64 {
        let foreground = relative_luminance(foreground);
        let background = relative_luminance(background);
        (foreground.max(background) + 0.05) / (foreground.min(background) + 0.05)
    }

    #[test]
    fn normal_muted_and_selection_text_meet_wcag_aa_in_every_builtin_theme() {
        for variant in [
            ThemeVariant::Dark,
            ThemeVariant::Light,
            ThemeVariant::HighContrast,
            ThemeVariant::ColorBlindSafe,
            ThemeVariant::Ansi256,
            ThemeVariant::Ansi16,
            ThemeVariant::Monochrome,
        ] {
            let theme = Theme::variant(variant);
            for background in [theme.surface.panel, theme.surface.overlay] {
                let ratio = contrast_ratio(theme.text.muted, background);
                assert!(
                    ratio >= 4.5,
                    "{variant:?}: muted text contrast {ratio:.2} against {background:?}"
                );
            }
            let ratio = contrast_ratio(theme.selection.foreground, theme.selection.background);
            assert!(ratio >= 4.5, "{variant:?}: selection contrast {ratio:.2}");
            assert_eq!(
                theme.selection_aware_text_style(true, theme.text.muted).fg,
                Some(theme.selection.foreground),
                "{variant:?}: selected child spans must override their normal token"
            );
        }
    }

    #[test]
    fn comments_and_focus_indicators_are_legible_in_every_builtin_theme() {
        for variant in [
            ThemeVariant::Dark,
            ThemeVariant::Light,
            ThemeVariant::HighContrast,
            ThemeVariant::ColorBlindSafe,
            ThemeVariant::Ansi256,
            ThemeVariant::Ansi16,
            ThemeVariant::Monochrome,
        ] {
            let theme = Theme::variant(variant);
            let comment = contrast_ratio(theme.syntax.comment, theme.surface.panel);
            assert!(comment >= 4.5, "{variant:?}: comment contrast {comment:.2}");
            let focus = contrast_ratio(theme.focus.active, theme.surface.background);
            assert!(
                focus >= 3.0,
                "{variant:?}: focus indicator contrast {focus:.2}"
            );
        }
    }

    /// Every variant used by real terminals must keep body text visible against
    /// the panel background, or the UI is unreadable — the core legibility
    /// invariant behind "every variant renders every widget legibly".
    #[test]
    fn text_is_never_the_same_color_as_its_background() {
        for v in [
            ThemeVariant::Dark,
            ThemeVariant::Light,
            ThemeVariant::HighContrast,
            ThemeVariant::ColorBlindSafe,
            ThemeVariant::Ansi256,
            ThemeVariant::Ansi16,
            ThemeVariant::Monochrome,
        ] {
            let t = Theme::variant(v);
            assert_ne!(
                t.text.primary, t.surface.panel,
                "{v:?}: primary text invisible"
            );
            assert_ne!(
                t.text.primary, t.surface.background,
                "{v:?}: primary text invisible"
            );
            assert_ne!(
                t.selection.foreground, t.selection.background,
                "{v:?}: selection text invisible"
            );
            // A focused pane must be distinguishable from an unfocused one.
            assert_ne!(t.focus.active, t.focus.inactive, "{v:?}: focus indistinct");
        }
    }

    #[test]
    fn everyday_selection_is_tonal_not_the_focus_accent() {
        for theme in [
            Theme::dark(),
            Theme::light(),
            Theme::color_blind_safe(),
            Theme::ansi256(),
            Theme::ansi16(),
        ] {
            assert_ne!(
                theme.selection.background, theme.focus.active,
                "ordinary list selection must not become a full-strength focus bar"
            );
        }
    }

    /// In the colored variants, added and removed diff lines must not collapse to
    /// the same hue, and success/error must differ — the semantic contrast the
    /// tokens exist to guarantee.
    #[test]
    fn colored_variants_keep_semantic_pairs_distinct() {
        for v in [
            ThemeVariant::Dark,
            ThemeVariant::Light,
            ThemeVariant::HighContrast,
            ThemeVariant::ColorBlindSafe,
            ThemeVariant::Ansi256,
            ThemeVariant::Ansi16,
        ] {
            let t = Theme::variant(v);
            assert_ne!(
                t.diff.added, t.diff.removed,
                "{v:?}: added/removed identical"
            );
            assert_ne!(
                t.status.success, t.status.error,
                "{v:?}: success/error identical"
            );
        }
    }

    /// The color-blind-safe variant must avoid the pure red/green diff pairing —
    /// that is the whole point of the Okabe–Ito palette.
    #[test]
    fn color_blind_safe_avoids_pure_red_green_for_diffs() {
        let t = Theme::color_blind_safe();
        assert_ne!(t.diff.added, Color::Rgb(0x00, 0xff, 0x00));
        assert_ne!(t.diff.removed, Color::Rgb(0xff, 0x00, 0x00));
        // Added is bluish-green, removed is vermillion (both from Okabe–Ito).
        assert_eq!(t.diff.added, Color::Rgb(0x00, 0x9e, 0x73));
        assert_eq!(t.diff.removed, Color::Rgb(0xd5, 0x5e, 0x00));
    }

    /// Every depth must resolve every syntax slot to a colour visible on its panel,
    /// and light must differ from dark — the expanded palette is real everywhere.
    #[test]
    fn every_depth_resolves_every_syntax_slot() {
        for v in [
            ThemeVariant::Dark,
            ThemeVariant::Light,
            ThemeVariant::HighContrast,
            ThemeVariant::ColorBlindSafe,
            ThemeVariant::Ansi256,
            ThemeVariant::Ansi16,
            ThemeVariant::Monochrome,
        ] {
            let t = Theme::variant(v);
            for c in [
                t.syntax.keyword,
                t.syntax.literal,
                t.syntax.string,
                t.syntax.comment,
                t.syntax.r#type,
                t.syntax.function,
                t.syntax.operator,
                t.syntax.constant,
                t.syntax.punctuation,
            ] {
                assert_ne!(
                    c, t.surface.panel,
                    "{v:?}: a syntax slot is invisible on the panel"
                );
            }
        }
        // A sensible light/dark distinction on the new slots.
        assert_ne!(Theme::dark().syntax.r#type, Theme::light().syntax.r#type);
        assert_ne!(
            Theme::dark().syntax.function,
            Theme::light().syntax.function
        );
    }

    /// The user container surface: distinct on the five raised-surface depths;
    /// deliberately == panel on ansi16/monochrome (the accent-bar fallback).
    #[test]
    fn surface_user_is_distinct_where_a_raised_surface_exists() {
        for v in [
            ThemeVariant::Dark,
            ThemeVariant::Light,
            ThemeVariant::HighContrast,
            ThemeVariant::ColorBlindSafe,
            ThemeVariant::Ansi256,
        ] {
            let t = Theme::variant(v);
            assert_ne!(
                t.surface.user, t.surface.panel,
                "{v:?}: user surface not distinct"
            );
        }
        assert_eq!(Theme::ansi16().surface.user, Theme::ansi16().surface.panel);
        assert_eq!(
            Theme::monochrome().surface.user,
            Theme::monochrome().surface.panel
        );
    }

    /// The monochrome variant must use only grayscale (white/gray/black) — no
    /// chromatic color at all.
    #[test]
    fn monochrome_is_purely_grayscale() {
        let t = Theme::monochrome();
        let grayscale = [Color::White, Color::Gray, Color::DarkGray, Color::Black];
        for c in [
            t.status.info,
            t.status.success,
            t.status.warning,
            t.status.error,
            t.diff.added,
            t.diff.removed,
            t.syntax.keyword,
            t.syntax.r#type,
            t.syntax.function,
            t.syntax.operator,
            t.syntax.constant,
            t.syntax.punctuation,
            t.surface.user,
            t.agent.tool,
            t.focus.active,
        ] {
            assert!(
                grayscale.contains(&c),
                "monochrome used a chromatic color: {c:?}"
            );
        }
    }

    #[test]
    fn detect_reads_env_conventions() {
        assert_eq!(
            ColorDepth::from_env(None, Some("truecolor"), Some("xterm-256color")),
            ColorDepth::TrueColor,
            "COLORTERM=truecolor wins over TERM"
        );
        assert_eq!(
            ColorDepth::from_env(None, None, Some("xterm-256color")),
            ColorDepth::Ansi256
        );
        assert_eq!(
            ColorDepth::from_env(None, None, Some("xterm")),
            ColorDepth::Ansi16
        );
        assert_eq!(
            ColorDepth::from_env(None, None, Some("dumb")),
            ColorDepth::Monochrome
        );
        // NO_COLOR overrides everything.
        assert_eq!(
            ColorDepth::from_env(Some("1"), Some("truecolor"), Some("xterm-256color")),
            ColorDepth::Monochrome
        );
        // Empty NO_COLOR does NOT disable color (the spec: any non-empty value).
        assert_eq!(
            ColorDepth::from_env(Some(""), Some("truecolor"), None),
            ColorDepth::TrueColor
        );
    }

    #[test]
    fn select_picks_by_depth() {
        let none = ThemePreferences::default();
        assert_eq!(Theme::select(ColorDepth::TrueColor, none), Theme::dark());
        assert_eq!(Theme::select(ColorDepth::Ansi256, none), Theme::ansi256());
        assert_eq!(Theme::select(ColorDepth::Ansi16, none), Theme::ansi16());
        assert_eq!(
            Theme::select(ColorDepth::Monochrome, none),
            Theme::monochrome()
        );
    }

    #[test]
    fn select_honors_accessibility_prefs_over_depth() {
        let hc = ThemePreferences {
            high_contrast: true,
            ..Default::default()
        };
        assert_eq!(
            Theme::select(ColorDepth::TrueColor, hc),
            Theme::high_contrast()
        );
        let cb = ThemePreferences {
            color_blind_safe: true,
            ..Default::default()
        };
        assert_eq!(
            Theme::select(ColorDepth::TrueColor, cb),
            Theme::color_blind_safe()
        );
        // But a monochrome terminal cannot render high-contrast color — depth wins
        // there, since the distinct colors accessibility relies on aren't available.
        assert_eq!(
            Theme::select(ColorDepth::Monochrome, hc),
            Theme::monochrome()
        );
    }

    #[test]
    fn manual_override_always_wins() {
        let prefs = ThemePreferences {
            high_contrast: true,
            override_variant: Some(ThemeVariant::Light),
            ..Default::default()
        };
        // Override beats both the high-contrast pref and the true-color depth.
        assert_eq!(Theme::select(ColorDepth::TrueColor, prefs), Theme::light());
        assert_eq!(Theme::select(ColorDepth::Monochrome, prefs), Theme::light());
    }
}
