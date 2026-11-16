//! Calm, minimal palette (dark by default, follows the OS unless overridden in Settings), plus
//! the type and spacing scale. Spacing sits on a 4/8 px grid; density changes vertical rhythm only.

use gpui::{hsla, px, rgb, rgba, Hsla, Pixels, Rgba, WindowAppearance};
use hermes_core::conn::{Prefs, ThemePref};

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: Rgba,
    pub sidebar: Rgba,
    pub panel: Rgba,
    pub raised: Rgba,
    pub hover: Rgba,
    pub pressed: Rgba,
    pub selected: Rgba,
    pub border: Rgba,
    pub border_strong: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    pub bot_bubble: Rgba,
    pub accent: Rgba,
    pub accent_soft: Rgba,
    pub on_accent: Rgba,
    pub danger: Rgba,
    pub danger_soft: Rgba,
    pub ok: Rgba,
    pub warn: Rgba,
    pub input_bg: Rgba,
    pub skeleton: Rgba,
    pub scrim: Rgba,
    /// Base font size (px) and density multiplier.
    pub fs: f32,
    pub sp: f32,
}

pub const ACCENTS: &[(u32, &str)] = &[
    (0x3b82f6, "Blue"),
    (0x8b5cf6, "Violet"),
    (0xec4899, "Pink"),
    (0xef4444, "Red"),
    (0xf97316, "Orange"),
    (0x22c55e, "Green"),
    (0x14b8a6, "Teal"),
    (0x71717a, "Graphite"),
];

pub const DEFAULT_ACCENT: u32 = 0x3b82f6;

impl Palette {
    pub fn resolve(appearance: WindowAppearance, prefs: &Prefs, accent: u32) -> Palette {
        let dark = match prefs.theme {
            ThemePref::Dark => true,
            ThemePref::Light => false,
            ThemePref::System => !matches!(appearance, WindowAppearance::Light | WindowAppearance::VibrantLight),
        };
        let mut p = if dark { Palette::dark(accent) } else { Palette::light(accent) };
        p.fs = prefs.font_size();
        p.sp = prefs.space();
        p
    }

    /// Text size scaled by the font-size preference (designs use 14 px as the base).
    pub fn t(&self, size: f32) -> Pixels {
        px((size * self.fs / 14.0).round())
    }

    /// Vertical spacing scaled by density, snapped to the 4 px grid (minimum 4).
    pub fn g(&self, size: f32) -> Pixels {
        px(((size * self.sp / 4.0).round() * 4.0).max(4.0))
    }

    fn soft(accent: u32, alpha: u32) -> Rgba {
        rgba((accent << 8) | alpha)
    }

    pub fn dark(accent: u32) -> Palette {
        Palette {
            dark: true,
            bg: rgb(0x0e0e10),
            sidebar: rgb(0x141416),
            panel: rgb(0x1a1a1d),
            raised: rgb(0x202024),
            hover: rgba(0xffffff0f),
            pressed: rgba(0xffffff1a),
            selected: rgba(0xffffff14),
            border: rgba(0xffffff12),
            border_strong: rgba(0xffffff24),
            text: rgb(0xececf1),
            muted: rgb(0x9a9aa3),
            // ≥ 4.5:1 on bg and sidebar (WCAG AA for small text).
            faint: rgb(0x85858e),
            bot_bubble: rgb(0x232327),
            accent: rgb(accent),
            accent_soft: Self::soft(accent, 0x2e),
            on_accent: on_accent(accent),
            danger: rgb(0xf87171),
            danger_soft: rgba(0xf8717122),
            ok: rgb(0x4ade80),
            warn: rgb(0xfbbf24),
            input_bg: rgb(0x1c1c20),
            skeleton: rgba(0xffffff12),
            scrim: rgba(0x000000a6),
            fs: 14.0,
            sp: 1.0,
        }
    }

    pub fn light(accent: u32) -> Palette {
        Palette {
            dark: false,
            bg: rgb(0xffffff),
            sidebar: rgb(0xf7f7f8),
            panel: rgb(0xffffff),
            raised: rgb(0xf4f4f6),
            hover: rgba(0x0000000a),
            pressed: rgba(0x00000014),
            selected: rgba(0x0000000f),
            border: rgba(0x00000014),
            border_strong: rgba(0x00000024),
            text: rgb(0x16161a),
            muted: rgb(0x62626c),
            // ≥ 4.5:1 on bg and sidebar (WCAG AA for small text).
            faint: rgb(0x6e6e78),
            bot_bubble: rgb(0xf0f0f3),
            accent: rgb(accent),
            accent_soft: Self::soft(accent, 0x1f),
            on_accent: on_accent(accent),
            danger: rgb(0xdc2626),
            danger_soft: rgba(0xdc262614),
            ok: rgb(0x16a34a),
            warn: rgb(0xd97706),
            input_bg: rgb(0xf4f4f6),
            skeleton: rgba(0x0000000f),
            scrim: rgba(0x00000059),
            fs: 14.0,
            sp: 1.0,
        }
    }
}

/// WCAG relative luminance of `0xrrggbb`.
pub fn luminance(hex: u32) -> f32 {
    let ch = |s: u32| {
        let c = ((hex >> s) & 0xff) as f32 / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * ch(16) + 0.7152 * ch(8) + 0.0722 * ch(0)
}

/// WCAG contrast ratio between two `0xrrggbb` colours (1–21).
pub fn contrast(a: u32, b: u32) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

const ON_ACCENT_DARK: u32 = 0x111114;

/// Text on an accent fill (buttons, badges, the user bubble): white while it keeps at least
/// 3:1 (bold UI text), otherwise near-black. Orange, green, teal and light custom accents get
/// dark text.
pub fn on_accent(accent: u32) -> Rgba {
    rgb(on_accent_hex(accent))
}

pub fn on_accent_hex(accent: u32) -> u32 {
    if contrast(accent, 0xffffff) >= 3.0 || contrast(accent, 0xffffff) >= contrast(accent, ON_ACCENT_DARK) {
        0xffffff
    } else {
        ON_ACCENT_DARK
    }
}

/// Avatar colour: explicit `#rrggbb` from Bot Mode metadata, else a stable hue from the name.
pub fn avatar_color(hex: Option<u32>, seed: u32) -> Hsla {
    if let Some(h) = hex {
        return rgb(h).into();
    }
    hsla((seed % 360) as f32 / 360.0, 0.55, 0.52, 1.0)
}

#[cfg(test)]
mod contrast_tests {
    use super::*;

    fn hex(c: Rgba) -> u32 {
        let b = |v: f32| (v * 255.0).round() as u32;
        (b(c.r) << 16) | (b(c.g) << 8) | b(c.b)
    }

    #[test]
    fn faint_text_meets_aa_and_accents_stay_legible() {
        for p in [Palette::light(DEFAULT_ACCENT), Palette::dark(DEFAULT_ACCENT)] {
            for bg in [p.bg, p.sidebar] {
                assert!(contrast(hex(p.faint), hex(bg)) >= 4.5, "faint on {:06x}", hex(bg));
            }
        }
        for &(a, name) in ACCENTS.iter().chain([(0xfacc15, "custom yellow"), (0x0f172a, "custom navy")].iter()) {
            assert!(contrast(a, on_accent_hex(a)) >= 3.0, "{name}");
        }
        assert_eq!(on_accent_hex(0x3b82f6), 0xffffff, "the default blue keeps white text");
        assert_eq!(on_accent_hex(0xf97316), ON_ACCENT_DARK);
    }
}
