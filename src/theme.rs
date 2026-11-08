//! Colour themes.

use egui::Color32;

const fn hex(v: u32) -> Color32 {
    Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub foreground: Color32,
    pub background: Color32,
    pub cursor: Color32,
    pub selection: Color32,
    pub ansi: [Color32; 16],
}

struct Preset {
    name: &'static str,
    /// foreground, background, cursor, selection, then the 16 ANSI colours.
    colors: [u32; 20],
}

const PRESETS: &[Preset] = &[
    Preset {
        name: "Omega",
        colors: [
            0xd5dbe5, 0x10131a, 0x6fd3ff, 0x2e4a66, 0x1c2029, 0xf0616d, 0x7fd18b, 0xf2c66d,
            0x62a8f5, 0xc08bf0, 0x4fcbd6, 0xc2c9d6, 0x4d5566, 0xff8a94, 0xa3e6ad, 0xffdc94,
            0x8ec2ff, 0xd7b0fa, 0x7fe4ec, 0xf1f4f9,
        ],
    },
    Preset {
        name: "Solarized Dark",
        colors: [
            0x839496, 0x002b36, 0x93a1a1, 0x274642, 0x073642, 0xdc322f, 0x859900, 0xb58900,
            0x268bd2, 0xd33682, 0x2aa198, 0xeee8d5, 0x002b36, 0xcb4b16, 0x586e75, 0x657b83,
            0x839496, 0x6c71c4, 0x93a1a1, 0xfdf6e3,
        ],
    },
    Preset {
        name: "Nord",
        colors: [
            0xd8dee9, 0x2e3440, 0xd8dee9, 0x434c5e, 0x3b4252, 0xbf616a, 0xa3be8c, 0xebcb8b,
            0x81a1c1, 0xb48ead, 0x88c0d0, 0xe5e9f0, 0x4c566a, 0xbf616a, 0xa3be8c, 0xebcb8b,
            0x81a1c1, 0xb48ead, 0x8fbcbb, 0xeceff4,
        ],
    },
    Preset {
        name: "Gruvbox Dark",
        colors: [
            0xebdbb2, 0x282828, 0xebdbb2, 0x504945, 0x282828, 0xcc241d, 0x98971a, 0xd79921,
            0x458588, 0xb16286, 0x689d6a, 0xa89984, 0x928374, 0xfb4934, 0xb8bb26, 0xfabd2f,
            0x83a598, 0xd3869b, 0x8ec07c, 0xebdbb2,
        ],
    },
    Preset {
        name: "Dracula",
        colors: [
            0xf8f8f2, 0x282a36, 0xf8f8f2, 0x44475a, 0x21222c, 0xff5555, 0x50fa7b, 0xf1fa8c,
            0xbd93f9, 0xff79c6, 0x8be9fd, 0xf8f8f2, 0x6272a4, 0xff6e6e, 0x69ff94, 0xffffa5,
            0xd6acff, 0xff92df, 0xa4ffff, 0xffffff,
        ],
    },
];

pub fn names() -> impl Iterator<Item = &'static str> {
    PRESETS.iter().map(|p| p.name)
}

fn dim(c: Color32) -> Color32 {
    let scale = |v: u8| (v as f32 * 0.66) as u8;
    Color32::from_rgb(scale(c.r()), scale(c.g()), scale(c.b()))
}

impl Theme {
    pub fn named(name: &str, foreground: Option<[u8; 3]>, background: Option<[u8; 3]>) -> Self {
        let preset = PRESETS
            .iter()
            .find(|p| p.name == name)
            .unwrap_or(&PRESETS[0]);
        let c = preset.colors;
        let mut ansi = [Color32::BLACK; 16];
        for (slot, value) in ansi.iter_mut().zip(&c[4..]) {
            *slot = hex(*value);
        }
        let rgb = |v: [u8; 3]| Color32::from_rgb(v[0], v[1], v[2]);
        Self {
            foreground: foreground.map_or(hex(c[0]), rgb),
            background: background.map_or(hex(c[1]), rgb),
            cursor: hex(c[2]),
            selection: hex(c[3]),
            ansi,
        }
    }

    /// Looks up a colour by its terminal index: 0-255 are the xterm palette,
    /// and 256+ follow the order of alacritty's `NamedColor`.
    pub fn color(&self, index: usize) -> Color32 {
        match index {
            0..=15 => self.ansi[index],
            16..=231 => {
                let i = index - 16;
                let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
                Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
            }
            232..=255 => {
                let v = (8 + (index - 232) * 10) as u8;
                Color32::from_rgb(v, v, v)
            }
            256 => self.foreground,
            257 => self.background,
            258 => self.cursor,
            259..=266 => dim(self.ansi[index - 259]),
            267 => self.foreground,
            268 => dim(self.foreground),
            _ => self.foreground,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_palette_endpoints() {
        let theme = Theme::named("Omega", None, None);
        assert_eq!(theme.color(16), Color32::from_rgb(0, 0, 0));
        assert_eq!(theme.color(231), Color32::from_rgb(255, 255, 255));
        assert_eq!(theme.color(232), Color32::from_rgb(8, 8, 8));
        assert_eq!(theme.color(255), Color32::from_rgb(238, 238, 238));
    }

    #[test]
    fn unknown_theme_falls_back() {
        let theme = Theme::named("does not exist", None, None);
        assert_eq!(
            theme.background,
            Theme::named("Omega", None, None).background
        );
    }

    #[test]
    fn overrides_apply() {
        let theme = Theme::named("Nord", Some([1, 2, 3]), Some([4, 5, 6]));
        assert_eq!(theme.color(256), Color32::from_rgb(1, 2, 3));
        assert_eq!(theme.color(257), Color32::from_rgb(4, 5, 6));
    }
}
