//! Calm, minimal palette (dark by default, follows the OS appearance).

use gpui::{hsla, rgb, rgba, Hsla, Rgba, WindowAppearance};

#[derive(Clone, Copy)]
pub struct Palette {
    pub dark: bool,
    pub bg: Rgba,
    pub sidebar: Rgba,
    pub panel: Rgba,
    pub hover: Rgba,
    pub selected: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub faint: Rgba,
    pub bot_bubble: Rgba,
    pub accent: Rgba,
    pub on_accent: Rgba,
    pub notice_bg: Rgba,
    pub notice_border: Rgba,
    pub danger: Rgba,
    pub ok: Rgba,
    pub input_bg: Rgba,
}

impl Palette {
    pub fn for_appearance(a: WindowAppearance, accent: u32) -> Palette {
        match a {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Palette::light(accent),
            _ => Palette::dark(accent),
        }
    }

    pub fn dark(accent: u32) -> Palette {
        Palette {
            dark: true,
            bg: rgb(0x0e0e10),
            sidebar: rgb(0x151517),
            panel: rgb(0x1b1b1e),
            hover: rgba(0xffffff0d),
            selected: rgba(0xffffff17),
            border: rgba(0xffffff14),
            text: rgb(0xececf1),
            muted: rgb(0x9a9aa3),
            faint: rgb(0x6b6b74),
            bot_bubble: rgb(0x26262a),
            accent: rgb(accent),
            on_accent: rgb(0xffffff),
            notice_bg: rgba(0x8b5cf61f),
            notice_border: rgba(0x8b5cf666),
            danger: rgb(0xf87171),
            ok: rgb(0x4ade80),
            input_bg: rgb(0x1e1e22),
        }
    }

    pub fn light(accent: u32) -> Palette {
        Palette {
            dark: false,
            bg: rgb(0xffffff),
            sidebar: rgb(0xf6f6f8),
            panel: rgb(0xf2f2f5),
            hover: rgba(0x0000000a),
            selected: rgba(0x00000012),
            border: rgba(0x00000014),
            text: rgb(0x16161a),
            muted: rgb(0x6b6b76),
            faint: rgb(0x9b9ba5),
            bot_bubble: rgb(0xefeff2),
            accent: rgb(accent),
            on_accent: rgb(0xffffff),
            notice_bg: rgba(0x8b5cf614),
            notice_border: rgba(0x8b5cf655),
            danger: rgb(0xdc2626),
            ok: rgb(0x16a34a),
            input_bg: rgb(0xf4f4f6),
        }
    }
}

/// Avatar colour: explicit `#rrggbb` from Bot Mode metadata, else a stable hue from the name.
pub fn avatar_color(hex: Option<u32>, seed: u32) -> Hsla {
    if let Some(h) = hex {
        return rgb(h).into();
    }
    hsla((seed % 360) as f32 / 360.0, 0.55, 0.52, 1.0)
}

pub const DEFAULT_ACCENT: u32 = 0x3b82f6;
