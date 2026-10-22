use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{self, Color, NamedColor};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub const fn from_hex(hex: u32) -> Self {
        Self::new((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
    }

    fn dim(self) -> Self {
        let f = |c: u8| (c as u16 * 2 / 3) as u8;
        Self::new(f(self.r), f(self.g), f(self.b))
    }
}

impl From<ansi::Rgb> for Rgb {
    fn from(c: ansi::Rgb) -> Self {
        Self::new(c.r, c.g, c.b)
    }
}

/// 终端配色方案。256 色中 16 之后的部分按 xterm 标准公式计算。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub ansi: [Rgb; 16],
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
    pub selection: Rgb,
}

impl Default for Palette {
    fn default() -> Self {
        Self::dark()
    }
}

impl Palette {
    /// 主题未给出终端配色时使用的暗色方案。
    pub fn dark() -> Self {
        Self {
            ansi: [
                0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6,
                0x666666, 0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
            ]
            .map(Rgb::from_hex),
            foreground: Rgb::from_hex(0xd8dee9),
            background: Rgb::from_hex(0x1e2127),
            cursor: Rgb::from_hex(0xd8dee9),
            selection: Rgb::from_hex(0x3e4451),
        }
    }

    /// 主题未给出终端配色时使用的亮色方案。
    pub fn light() -> Self {
        Self {
            ansi: [
                0x383a42, 0xe45649, 0x50a14f, 0xc18401, 0x4078f2, 0xa626a4, 0x0184bc, 0xa0a1a7,
                0x696c77, 0xca1243, 0x3e8f3d, 0x986801, 0x3a6fe0, 0xb73ab5, 0x0997b3, 0xd3d4d8,
            ]
            .map(Rgb::from_hex),
            foreground: Rgb::from_hex(0x383a42),
            background: Rgb::from_hex(0xfafafa),
            cursor: Rgb::from_hex(0x526eff),
            selection: Rgb::from_hex(0xcfdcf7),
        }
    }

    fn indexed(&self, index: u8) -> Rgb {
        match index {
            0..=15 => self.ansi[index as usize],
            16..=231 => {
                let i = index - 16;
                let level = |v: u8| if v == 0 { 0 } else { v * 40 + 55 };
                Rgb::new(level(i / 36), level((i / 6) % 6), level(i % 6))
            }
            232..=255 => {
                let v = (index - 232) * 10 + 8;
                Rgb::new(v, v, v)
            }
        }
    }

    fn named(&self, color: NamedColor) -> Rgb {
        let idx = color as usize;
        match color {
            NamedColor::Foreground | NamedColor::BrightForeground => self.foreground,
            NamedColor::Background => self.background,
            NamedColor::Cursor => self.cursor,
            NamedColor::DimForeground => self.foreground.dim(),
            _ if idx < 16 => self.ansi[idx],
            // Dim 系列紧随 Cursor 之后，顺序与 Black..White 一致
            _ => self.ansi[idx - NamedColor::DimBlack as usize].dim(),
        }
    }

    /// 按 alacritty 的颜色表下标取色：0..256 为索引色，之后为前景/背景/光标等命名色。
    pub(crate) fn by_index(&self, index: usize) -> Rgb {
        match index {
            0..=255 => self.indexed(index as u8),
            256 => self.foreground,
            257 => self.background,
            _ => self.cursor,
        }
    }

    /// 解析单元格颜色。程序通过 OSC 4/10/11 设置的颜色优先于配色方案。
    pub(crate) fn resolve(&self, color: Color, overrides: &Colors) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb.into(),
            Color::Indexed(i) => overrides[i as usize].map_or_else(|| self.indexed(i), Rgb::from),
            Color::Named(n) => overrides[n].map_or_else(|| self.named(n), Rgb::from),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_cube_and_grayscale() {
        let p = Palette::default();
        assert_eq!(p.indexed(16), Rgb::new(0, 0, 0));
        assert_eq!(p.indexed(196), Rgb::new(255, 0, 0));
        assert_eq!(p.indexed(231), Rgb::new(255, 255, 255));
        assert_eq!(p.indexed(232), Rgb::new(8, 8, 8));
        assert_eq!(p.indexed(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn overrides_take_precedence() {
        let p = Palette::default();
        let mut o = Colors::default();
        o[NamedColor::Foreground] = Some(ansi::Rgb { r: 1, g: 2, b: 3 });
        assert_eq!(
            p.resolve(Color::Named(NamedColor::Foreground), &o),
            Rgb::new(1, 2, 3)
        );
        assert_eq!(
            p.resolve(Color::Named(NamedColor::DimRed), &Colors::default()),
            p.ansi[1].dim()
        );
    }
}
