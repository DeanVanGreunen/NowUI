//! Tailwind CSS v4 default-theme design tokens, as plain lookup functions.
//!
//! This module only maps *token strings* (`"4"`, `"blue-500"`, `"lg"`, ...) to
//! concrete numbers/colors — it knows nothing about the parser or which style
//! key it's being used for. `nowui-runtime`'s semantic pass is what decides
//! which token table applies to which class family.
//!
//! Values mirror Tailwind's default theme (assuming a 16px root, i.e.
//! `1rem == 16px`, matching this renderer's fixed-pixel model). No 3D
//! transforms, filters/shadows, `@keyframes` animations, container queries,
//! or `dark:`/`group-*`/`peer-*` variants — see CLAUDE.md for what's in scope.

use crate::geometry::Color;

/// Tailwind's default spacing scale, in pixels (`1` unit == `0.25rem` == `4px`).
/// Used by `p-*`, `m-*`, `gap-*`, `w-*`, `h-*`, `inset-*`, `translate-*`, etc.
pub fn spacing(token: &str) -> Option<f32> {
    if token == "px" {
        return Some(1.0);
    }
    let n: f32 = token.parse().ok()?;
    Some(n * 4.0)
}

/// A bare fraction like `1/2`, `2/3` — used by `w-1/2`, `h-1/3`, etc. Returns
/// the fraction as 0..=1.
pub fn fraction(token: &str) -> Option<f32> {
    let (num, den) = token.split_once('/')?;
    let num: f32 = num.parse().ok()?;
    let den: f32 = den.parse().ok()?;
    if den == 0.0 {
        None
    } else {
        Some(num / den)
    }
}

/// Font size scale: `text-{token}` -> (font size px, line height px).
pub fn font_size(token: &str) -> Option<(f32, f32)> {
    Some(match token {
        "xs" => (12.0, 16.0),
        "sm" => (14.0, 20.0),
        "base" => (16.0, 24.0),
        "lg" => (18.0, 28.0),
        "xl" => (20.0, 28.0),
        "2xl" => (24.0, 32.0),
        "3xl" => (30.0, 36.0),
        "4xl" => (36.0, 40.0),
        "5xl" => (48.0, 48.0),
        "6xl" => (60.0, 60.0),
        "7xl" => (72.0, 72.0),
        "8xl" => (96.0, 96.0),
        "9xl" => (128.0, 128.0),
        _ => return None,
    })
}

/// `font-{token}` -> numeric weight.
pub fn font_weight(token: &str) -> Option<u16> {
    Some(match token {
        "thin" => 100,
        "extralight" => 200,
        "light" => 300,
        "normal" => 400,
        "medium" => 500,
        "semibold" => 600,
        "bold" => 700,
        "extrabold" => 800,
        "black" => 900,
        _ => return None,
    })
}

/// `leading-{token}` -> line height in px, given the current font size (for
/// the unitless line-height keywords Tailwind expresses as a multiplier).
pub fn leading(token: &str, font_size_px: f32) -> Option<f32> {
    Some(match token {
        "none" => font_size_px * 1.0,
        "tight" => font_size_px * 1.25,
        "snug" => font_size_px * 1.375,
        "normal" => font_size_px * 1.5,
        "relaxed" => font_size_px * 1.625,
        "loose" => font_size_px * 2.0,
        _ => return Some(spacing(token)?), // leading-6, leading-[24px], etc.
    })
}

/// `tracking-{token}` -> letter spacing in px (given a 16px reference size,
/// matching Tailwind's rem-based values).
pub fn tracking(token: &str) -> Option<f32> {
    Some(match token {
        "tighter" => -0.8,
        "tight" => -0.4,
        "normal" => 0.0,
        "wide" => 0.4,
        "wider" => 0.8,
        "widest" => 1.6,
        _ => return None,
    })
}

/// `rounded-{token}` -> radius in px.
pub fn radius(token: &str) -> Option<f32> {
    Some(match token {
        "none" => 0.0,
        "sm" => 4.0,
        "" | "default" => 6.0,
        "md" => 8.0,
        "lg" => 12.0,
        "xl" => 16.0,
        "2xl" => 24.0,
        "3xl" => 32.0,
        "full" => 9999.0,
        _ => return None,
    })
}

/// `border{-side}-{token}` -> width in px.
pub fn border_width(token: &str) -> Option<f32> {
    Some(match token {
        "" | "default" => 1.0,
        "0" => 0.0,
        "2" => 2.0,
        "4" => 4.0,
        "8" => 8.0,
        _ => return None,
    })
}

/// `opacity-{token}` -> 0.0..=1.0.
pub fn opacity(token: &str) -> Option<f32> {
    let n: f32 = token.parse().ok()?;
    Some((n / 100.0).clamp(0.0, 1.0))
}

/// `duration-{token}` -> milliseconds.
pub fn duration_ms(token: &str) -> Option<f32> {
    token.parse().ok()
}

/// `delay-{token}` -> milliseconds.
pub fn delay_ms(token: &str) -> Option<f32> {
    token.parse().ok()
}

/// Standard easing curves, as cubic-bezier control points `(x1, y1, x2, y2)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Easing {
    Linear,
    In,
    Out,
    InOut,
}

impl Easing {
    /// `ease-{token}` (bare `ease` == `ease` in CSS, i.e. the default curve).
    pub fn from_token(token: &str) -> Option<Easing> {
        Some(match token {
            "linear" => Easing::Linear,
            "in" => Easing::In,
            "out" => Easing::Out,
            "in-out" => Easing::InOut,
            "" => Easing::InOut,
            _ => return None,
        })
    }

    /// Ease `t` (0..=1 linear progress) into 0..=1 eased progress.
    pub fn apply(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Easing::Linear => t,
            Easing::In => t * t,
            Easing::Out => 1.0 - (1.0 - t) * (1.0 - t),
            Easing::InOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
                }
            }
        }
    }
}

/// Standard breakpoints (`sm:`, `md:`, `lg:`, `xl:`, `2xl:`), min-width in px.
pub fn breakpoint(token: &str) -> Option<u32> {
    Some(match token {
        "sm" => 640,
        "md" => 768,
        "lg" => 1024,
        "xl" => 1280,
        "2xl" => 1536,
        _ => return None,
    })
}

/// Resolve a color token: `"white"`, `"black"`, `"transparent"`, a bare
/// palette name (`"blue"` == `blue-500`), or `"{family}-{shade}"`.
pub fn color(token: &str) -> Option<Color> {
    match token {
        "white" => return Some(Color::rgb(255, 255, 255)),
        "black" => return Some(Color::rgb(0, 0, 0)),
        "transparent" => return Some(Color::TRANSPARENT),
        _ => {}
    }
    let (family, shade) = match token.rsplit_once('-') {
        Some((f, s)) if s.chars().all(|c| c.is_ascii_digit()) => (f, s),
        _ => (token, "500"),
    };
    let hex = palette(family, shade)?;
    Color::from_hex(hex)
}

/// The default Tailwind v4 color palette: 22 families x 11 shades.
fn palette(family: &str, shade: &str) -> Option<&'static str> {
    let row: [&str; 11] = match family {
        "slate" => ["#f8fafc", "#f1f5f9", "#e2e8f0", "#cbd5e1", "#94a3b8", "#64748b", "#475569", "#334155", "#1e293b", "#0f172a", "#020617"],
        "gray" => ["#f9fafb", "#f3f4f6", "#e5e7eb", "#d1d5db", "#9ca3af", "#6b7280", "#4b5563", "#374151", "#1f2937", "#111827", "#030712"],
        "zinc" => ["#fafafa", "#f4f4f5", "#e4e4e7", "#d4d4d8", "#a1a1aa", "#71717a", "#52525b", "#3f3f46", "#27272a", "#18181b", "#09090b"],
        "neutral" => ["#fafafa", "#f5f5f5", "#e5e5e5", "#d4d4d4", "#a3a3a3", "#737373", "#525252", "#404040", "#262626", "#171717", "#0a0a0a"],
        "stone" => ["#fafaf9", "#f5f5f4", "#e7e5e4", "#d6d3d1", "#a8a29e", "#78716c", "#57534e", "#44403c", "#292524", "#1c1917", "#0c0a09"],
        "red" => ["#fef2f2", "#fee2e2", "#fecaca", "#fca5a5", "#f87171", "#ef4444", "#dc2626", "#b91c1c", "#991b1b", "#7f1d1d", "#450a0a"],
        "orange" => ["#fff7ed", "#ffedd5", "#fed7aa", "#fdba74", "#fb923c", "#f97316", "#ea580c", "#c2410c", "#9a3412", "#7c2d12", "#431407"],
        "amber" => ["#fffbeb", "#fef3c7", "#fde68a", "#fcd34d", "#fbbf24", "#f59e0b", "#d97706", "#b45309", "#92400e", "#78350f", "#451a03"],
        "yellow" => ["#fefce8", "#fef9c3", "#fef08a", "#fde047", "#facc15", "#eab308", "#ca8a04", "#a16207", "#854d0e", "#713f12", "#422006"],
        "lime" => ["#f7fee7", "#ecfccb", "#d9f99d", "#bef264", "#a3e635", "#84cc16", "#65a30d", "#4d7c0f", "#3f6212", "#365314", "#1a2e05"],
        "green" => ["#f0fdf4", "#dcfce7", "#bbf7d0", "#86efac", "#4ade80", "#22c55e", "#16a34a", "#15803d", "#166534", "#14532d", "#052e16"],
        "emerald" => ["#ecfdf5", "#d1fae5", "#a7f3d0", "#6ee7b7", "#34d399", "#10b981", "#059669", "#047857", "#065f46", "#064e3b", "#022c22"],
        "teal" => ["#f0fdfa", "#ccfbf1", "#99f6e4", "#5eead4", "#2dd4bf", "#14b8a6", "#0d9488", "#0f766e", "#115e59", "#134e4a", "#042f2e"],
        "cyan" => ["#ecfeff", "#cffafe", "#a5f3fc", "#67e8f9", "#22d3ee", "#06b6d4", "#0891b2", "#0e7490", "#155e75", "#164e63", "#083344"],
        "sky" => ["#f0f9ff", "#e0f2fe", "#bae6fd", "#7dd3fc", "#38bdf8", "#0ea5e9", "#0284c7", "#0369a1", "#075985", "#0c4a6e", "#082f49"],
        "blue" => ["#eff6ff", "#dbeafe", "#bfdbfe", "#93c5fd", "#60a5fa", "#3b82f6", "#2563eb", "#1d4ed8", "#1e40af", "#1e3a8a", "#172554"],
        "indigo" => ["#eef2ff", "#e0e7ff", "#c7d2fe", "#a5b4fc", "#818cf8", "#6366f1", "#4f46e5", "#4338ca", "#3730a3", "#312e81", "#1e1b4b"],
        "violet" => ["#f5f3ff", "#ede9fe", "#ddd6fe", "#c4b5fd", "#a78bfa", "#8b5cf6", "#7c3aed", "#6d28d9", "#5b21b6", "#4c1d95", "#2e1065"],
        "purple" => ["#faf5ff", "#f3e8ff", "#e9d5ff", "#d8b4fe", "#c084fc", "#a855f7", "#9333ea", "#7e22ce", "#6b21a8", "#581c87", "#3b0764"],
        "fuchsia" => ["#fdf4ff", "#fae8ff", "#f5d0fe", "#f0abfc", "#e879f9", "#d946ef", "#c026d3", "#a21caf", "#86198f", "#701a75", "#4a044e"],
        "pink" => ["#fdf2f8", "#fce7f3", "#fbcfe8", "#f9a8d4", "#f472b6", "#ec4899", "#db2777", "#be185d", "#9d174d", "#831843", "#500724"],
        "rose" => ["#fff1f2", "#ffe4e6", "#fecdd3", "#fda4af", "#fb7185", "#f43f5e", "#e11d48", "#be123c", "#9f1239", "#881337", "#4c0519"],
        _ => return None,
    };
    let idx = match shade {
        "50" => 0,
        "100" => 1,
        "200" => 2,
        "300" => 3,
        "400" => 4,
        "500" => 5,
        "600" => 6,
        "700" => 7,
        "800" => 8,
        "900" => 9,
        "950" => 10,
        _ => return None,
    };
    Some(row[idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spacing_scale_matches_quarter_rem() {
        assert_eq!(spacing("4"), Some(16.0));
        assert_eq!(spacing("0.5"), Some(2.0));
        assert_eq!(spacing("px"), Some(1.0));
    }

    #[test]
    fn palette_resolves_family_and_shade() {
        assert_eq!(color("blue-600"), Color::from_hex("#2563eb"));
        assert_eq!(color("blue"), Color::from_hex("#3b82f6"));
        assert_eq!(color("white"), Some(Color::rgb(255, 255, 255)));
        assert_eq!(color("nope-500"), None);
    }

    #[test]
    fn easing_curves_stay_in_unit_range() {
        for e in [Easing::Linear, Easing::In, Easing::Out, Easing::InOut] {
            assert!((0.0..=1.0).contains(&e.apply(0.25)));
            assert_eq!(e.apply(0.0), 0.0);
            assert!((e.apply(1.0) - 1.0).abs() < 1e-4);
        }
    }
}
