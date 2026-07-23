//! Backend-agnostic geometry and color types.

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Size {
    pub w: f32,
    pub h: f32,
}

impl Size {
    pub fn new(w: f32, h: f32) -> Self {
        Self { w, h }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.x + self.w && p.y >= self.y && p.y <= self.y + self.h
    }

    /// Shrink by per-side insets (top, right, bottom, left).
    pub fn inset(&self, e: Edges) -> Rect {
        Rect {
            x: self.x + e.left,
            y: self.y + e.top,
            w: (self.w - e.left - e.right).max(0.0),
            h: (self.h - e.top - e.bottom).max(0.0),
        }
    }
}

/// Straight (non-premultiplied) RGBA, 0..=255.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color { r: 0, g: 0, b: 0, a: 0 };
    pub const WHITE: Color = Color { r: 255, g: 255, b: 255, a: 255 };
    pub const BLACK: Color = Color { r: 0, g: 0, b: 0, a: 255 };

    pub fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    /// Parse `#RRGGBB` or `#RGB`. Returns `None` on malformed input.
    pub fn from_hex(s: &str) -> Option<Color> {
        let s = s.strip_prefix('#').unwrap_or(s);
        let parse = |slice: &str| u8::from_str_radix(slice, 16).ok();
        match s.len() {
            6 => Some(Color {
                r: parse(&s[0..2])?,
                g: parse(&s[2..4])?,
                b: parse(&s[4..6])?,
                a: 255,
            }),
            3 => {
                let dup = |c: &str| u8::from_str_radix(&c.repeat(2), 16).ok();
                Some(Color {
                    r: dup(&s[0..1])?,
                    g: dup(&s[1..2])?,
                    b: dup(&s[2..3])?,
                    a: 255,
                })
            }
            _ => None,
        }
    }
}

/// Per-side spacing (padding / margin).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Edges {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl Edges {
    pub fn all(v: f32) -> Self {
        Self { top: v, right: v, bottom: v, left: v }
    }

    /// CSS-style shorthand: 1, 2, 3, or 4 values.
    pub fn parse(s: &str) -> Edges {
        let nums: Vec<f32> = s
            .split_whitespace()
            .map(|t| t.trim_end_matches("px").parse().unwrap_or(0.0))
            .collect();
        match nums.as_slice() {
            [a] => Edges::all(*a),
            [v, h] => Edges { top: *v, right: *h, bottom: *v, left: *h },
            [t, h, b] => Edges { top: *t, right: *h, bottom: *b, left: *h },
            [t, r, b, l] => Edges { top: *t, right: *r, bottom: *b, left: *l },
            _ => Edges::default(),
        }
    }
}
