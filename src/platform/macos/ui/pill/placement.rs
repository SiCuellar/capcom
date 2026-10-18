//! Pure pill placement geometry (no AppKit): where the capsule goes, given an anchor found via
//! the Accessibility API (see `anchor.rs`) and the screen layout.
//!
//! Coordinate systems:
//! - **AX**: global, origin at the top-left of the primary screen, y grows downward.
//! - **Cocoa**: global, origin at the bottom-left of the primary screen, y grows upward.
//!   [`ax_to_cocoa`] flips using the primary screen's height (`screens[0]`).
//!
//! Rules (all in Cocoa space, capsule rect = the visible pill, without the shadow margin):
//! - Caret / small field: capsule ~[`GAP`] pt **below** the anchor, horizontally centered on it;
//!   **above** the anchor if it would leave the visible frame at the bottom; clamped fully
//!   inside the visible frame of the screen containing the anchor. Never overlaps the anchor
//!   line unless neither side fits (then clamped below).
//! - Tall field (> half the visible frame height, e.g. a whole editor or web area) and window:
//!   bottom-center **inside** the visible portion of the element.
//! - Mouse (no AX info / `pill_position = "bottom"`): bottom-center of the visible frame of the
//!   screen under the mouse (the original behavior).

/// Gap between the anchor (caret line / field edge) and the capsule.
pub const GAP: f64 = 8.0;
/// Gap between the capsule and the bottom edge of the area it sits inside (visible frame,
/// window, tall field).
pub const BOTTOM_GAP: f64 = 14.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }
    pub fn max_x(&self) -> f64 {
        self.x + self.w
    }
    pub fn max_y(&self) -> f64 {
        self.y + self.h
    }
    pub fn mid_x(&self) -> f64 {
        self.x + self.w / 2.0
    }
    pub fn mid_y(&self) -> f64 {
        self.y + self.h / 2.0
    }
    /// Half-open containment (`[x, max_x) x [y, max_y)`).
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.max_x() && py >= self.y && py < self.max_y()
    }
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = self.max_x().min(o.max_x());
        let y1 = self.max_y().min(o.max_y());
        (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
    }
    /// Squared distance from a point to the rect (0 inside).
    fn dist2(&self, px: f64, py: f64) -> f64 {
        let dx = (self.x - px).max(0.0).max(px - self.max_x());
        let dy = (self.y - py).max(0.0).max(py - self.max_y());
        dx * dx + dy * dy
    }
    fn is_finite(&self) -> bool {
        self.x.is_finite() && self.y.is_finite() && self.w.is_finite() && self.h.is_finite()
    }
}

/// One display, Cocoa coordinates. `screens[0]` must be the primary screen (menu bar, origin 0,0),
/// matching `NSScreen.screens` order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenGeom {
    pub frame: Rect,
    pub visible: Rect,
}

/// Where to anchor the pill. Caret/Field/Window rects are raw AX (top-left origin) rects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    Caret(Rect),
    Field(Rect),
    /// A field that fills most of its window or screen (editor, terminal, web area): the pill
    /// goes inside it, bottom-center of its visible portion.
    LargeField(Rect),
    Window(Rect),
    /// Cocoa mouse location.
    Mouse(f64, f64),
}

impl Anchor {
    pub fn strategy(&self) -> &'static str {
        match self {
            Anchor::Caret(_) => "caret",
            Anchor::Field(_) | Anchor::LargeField(_) => "field",
            Anchor::Window(_) => "window",
            Anchor::Mouse(..) => "screen",
        }
    }
}

/// How the capsule relates to the anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Below,
    Above,
    /// Neither side fit; clamped into the visible frame.
    Clamped,
    /// Bottom-center inside the element / window / visible frame.
    InsideBottom,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Capsule rect, Cocoa coordinates.
    pub capsule: Rect,
    /// Index into the `screens` slice.
    pub screen: usize,
    pub side: Side,
}

/// Anchor for a focused element without a caret: an element filling >= 60% of its window's
/// height (terminal, editor, web area) is "large" (pill inside it); otherwise an editable field
/// gets the pill below/above it, and a small non-editable element (button, heading, ...) is no
/// anchor at all (`None`: use the window).
pub fn field_anchor(field: Rect, window: Option<Rect>, editable: bool) -> Option<Anchor> {
    match window {
        Some(win) if win.h > 0.0 && field.h >= 0.6 * win.h => Some(Anchor::LargeField(field)),
        _ if editable => Some(Anchor::Field(field)),
        _ => None,
    }
}

/// AX (top-left origin) -> Cocoa (bottom-left origin).
pub fn ax_to_cocoa(r: Rect, primary_h: f64) -> Rect {
    Rect::new(r.x, primary_h - r.y - r.h, r.w, r.h)
}

/// Screen whose frame contains the point; else the nearest one. `None` only if `screens` is empty.
pub fn screen_for_point(screens: &[ScreenGeom], px: f64, py: f64) -> Option<usize> {
    screens.iter().position(|s| s.frame.contains(px, py)).or_else(|| {
        screens
            .iter()
            .enumerate()
            .min_by(|a, b| a.1.frame.dist2(px, py).total_cmp(&b.1.frame.dist2(px, py)))
            .map(|(i, _)| i)
    })
}

fn primary_h(screens: &[ScreenGeom]) -> f64 {
    screens.first().map(|s| s.frame.h).unwrap_or(0.0)
}

/// Plausible caret: finite, positive height (width may be 0 for an insertion point), not
/// absurdly tall, not the all-zero rect Chromium returns on failure, and its center on a screen.
pub fn valid_caret_ax(r: Rect, screens: &[ScreenGeom]) -> bool {
    r.is_finite()
        && r.h > 0.0
        && r.h <= 400.0
        && r.w >= 0.0
        && !(r.x == 0.0 && r.y == 0.0)
        && on_some_screen(ax_to_cocoa(r, primary_h(screens)), screens)
}

/// Plausible element/window frame: finite, non-empty, intersects some screen.
pub fn valid_frame_ax(r: Rect, screens: &[ScreenGeom]) -> bool {
    r.is_finite() && r.w >= 1.0 && r.h >= 1.0 && {
        let c = ax_to_cocoa(r, primary_h(screens));
        screens.iter().any(|s| s.frame.intersect(&c).is_some())
    }
}

/// Caret from the bounds of the character just after the insertion point: its left edge.
pub fn caret_from_char_after(r: Rect) -> Rect {
    Rect::new(r.x, r.y, 0.0, r.h)
}

/// Caret from the bounds of the character just before the insertion point: its right edge, or,
/// when that character is a line break, the start of the next line (x from the zero-length
/// rect if there is one, which has the right x even where its y is off).
pub fn caret_from_char_before(r: Rect, newline: bool, len0: Option<Rect>) -> Rect {
    if newline {
        Rect::new(len0.map(|z| z.x).unwrap_or(r.x), r.max_y(), 0.0, r.h)
    } else {
        Rect::new(r.max_x(), r.y, 0.0, r.h)
    }
}

/// Caret from a selected-text-marker-range rect (collapsed: the caret itself; a selection: its
/// end edge).
pub fn caret_from_marker(r: Rect) -> Rect {
    Rect::new(r.max_x(), r.y, 0.0, r.h)
}

/// AppKit's NSTextView reports zero-length range bounds one line height too high (AX coords).
/// When such a rect lies entirely above the focused field it is certainly off: move it down a
/// line. (Elsewhere it cannot be detected, which is why character rects are preferred.)
pub fn fix_len0(r: Rect, field: Option<Rect>) -> Rect {
    match field {
        Some(f) if r.h > 0.0 && r.max_y() <= f.y + 0.5 && r.max_y() > f.y - r.h - 0.5 => Rect::new(r.x, r.y + r.h, r.w, r.h),
        _ => r,
    }
}

fn on_some_screen(c: Rect, screens: &[ScreenGeom]) -> bool {
    screens.iter().any(|s| s.frame.contains(c.mid_x(), c.mid_y()))
}

fn clamp(v: f64, lo: f64, hi: f64) -> f64 {
    if hi < lo {
        lo
    } else {
        v.clamp(lo, hi)
    }
}

/// Capsule next to `a` (Cocoa): below, else above, else clamped.
fn beside(a: Rect, vf: Rect, w: f64, h: f64) -> (Rect, Side) {
    let x = clamp(a.mid_x() - w / 2.0, vf.x, vf.max_x() - w);
    let below = a.y - GAP - h;
    if below >= vf.y {
        return (Rect::new(x, below, w, h), Side::Below);
    }
    let above = a.max_y() + GAP;
    if above + h <= vf.max_y() {
        return (Rect::new(x, above, w, h), Side::Above);
    }
    (Rect::new(x, clamp(below, vf.y, vf.max_y() - h), w, h), Side::Clamped)
}

/// Capsule at the bottom-center inside `area` (Cocoa), restricted to the visible frame.
fn inside_bottom(area: Rect, vf: Rect, w: f64, h: f64) -> Rect {
    let a = area.intersect(&vf).unwrap_or(vf);
    let x = clamp(a.mid_x() - w / 2.0, vf.x, vf.max_x() - w);
    let y = clamp(a.y + BOTTOM_GAP, vf.y, vf.max_y() - h);
    Rect::new(x, y, w, h)
}

/// Compute the capsule rect for `anchor` with a `w` x `h` capsule.
pub fn place(anchor: Anchor, screens: &[ScreenGeom], w: f64, h: f64) -> Option<Placement> {
    let ph = primary_h(screens);
    let (c, px, py) = match anchor {
        Anchor::Mouse(x, y) => (Rect::new(x, y, 0.0, 0.0), x, y),
        Anchor::Caret(r) | Anchor::Field(r) | Anchor::LargeField(r) | Anchor::Window(r) => {
            let c = ax_to_cocoa(r, ph);
            (c, c.mid_x(), c.mid_y())
        }
    };
    let screen = screen_for_point(screens, px, py)?;
    let vf = screens[screen].visible;
    let (capsule, side) = match anchor {
        Anchor::Caret(_) => beside(c, vf, w, h),
        Anchor::LargeField(_) => (inside_bottom(c, vf, w, h), Side::InsideBottom),
        Anchor::Field(_) if c.h > vf.h / 2.0 => (inside_bottom(c, vf, w, h), Side::InsideBottom),
        Anchor::Field(_) => beside(c, vf, w, h),
        Anchor::Window(_) => (inside_bottom(c, vf, w, h), Side::InsideBottom),
        Anchor::Mouse(..) => (inside_bottom(vf, vf, w, h), Side::InsideBottom),
    };
    Some(Placement { capsule, screen, side })
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: f64 = 112.0;
    const H: f64 = 32.0;

    /// 1512x982 primary (menu bar 33, dock 70 at the bottom) + a 1920x1080 screen to the right,
    /// bottom-aligned, no dock.
    fn screens() -> Vec<ScreenGeom> {
        vec![
            ScreenGeom { frame: Rect::new(0.0, 0.0, 1512.0, 982.0), visible: Rect::new(0.0, 70.0, 1512.0, 879.0) },
            ScreenGeom {
                frame: Rect::new(1512.0, 0.0, 1920.0, 1080.0),
                visible: Rect::new(1512.0, 0.0, 1920.0, 1055.0),
            },
        ]
    }

    fn inside(r: Rect, vf: Rect) -> bool {
        r.x >= vf.x - 1e-9 && r.max_x() <= vf.max_x() + 1e-9 && r.y >= vf.y - 1e-9 && r.max_y() <= vf.max_y() + 1e-9
    }

    #[test]
    fn flip() {
        let c = ax_to_cocoa(Rect::new(100.0, 200.0, 2.0, 18.0), 982.0);
        assert_eq!(c, Rect::new(100.0, 982.0 - 218.0, 2.0, 18.0));
        // Involution.
        assert_eq!(ax_to_cocoa(c, 982.0), Rect::new(100.0, 200.0, 2.0, 18.0));
        // A screen above the primary has negative AX y and Cocoa y above the primary's height.
        let up = ax_to_cocoa(Rect::new(0.0, -500.0, 10.0, 20.0), 982.0);
        assert_eq!(up.y, 982.0 + 480.0);
    }

    #[test]
    fn caret_below_centered() {
        let s = screens();
        let p = place(Anchor::Caret(Rect::new(400.0, 300.0, 0.0, 18.0)), &s, W, H).unwrap();
        assert_eq!(p.screen, 0);
        assert_eq!(p.side, Side::Below);
        // Caret Cocoa bottom = 982-318 = 664; capsule top = 664 - GAP.
        assert_eq!(p.capsule.max_y(), 664.0 - GAP);
        assert_eq!(p.capsule.mid_x(), 400.0);
        assert!(inside(p.capsule, s[0].visible));
    }

    #[test]
    fn caret_near_bottom_goes_above_without_covering_line() {
        let s = screens();
        // Caret line at AX y 880..898 -> Cocoa 84..102; below would need y >= 70 + ... -> no room.
        let caret = Rect::new(700.0, 880.0, 1.0, 18.0);
        let p = place(Anchor::Caret(caret), &s, W, H).unwrap();
        assert_eq!(p.side, Side::Above);
        let c = ax_to_cocoa(caret, 982.0);
        assert_eq!(p.capsule.y, c.max_y() + GAP);
        assert!(p.capsule.y > c.max_y(), "must not cover the caret line");
        assert!(inside(p.capsule, s[0].visible));
    }

    #[test]
    fn clamps_horizontally_at_screen_edges() {
        let s = screens();
        let l = place(Anchor::Caret(Rect::new(3.0, 300.0, 0.0, 18.0)), &s, W, H).unwrap();
        assert_eq!(l.capsule.x, 0.0);
        let r = place(Anchor::Caret(Rect::new(1510.0, 300.0, 0.0, 18.0)), &s, W, H).unwrap();
        assert_eq!(r.capsule.max_x(), 1512.0);
        assert_eq!(r.screen, 0);
    }

    #[test]
    fn caret_near_top_under_menu_bar_stays_below() {
        let s = screens();
        let p = place(Anchor::Caret(Rect::new(500.0, 40.0, 0.0, 16.0)), &s, W, H).unwrap();
        assert_eq!(p.side, Side::Below);
        assert!(inside(p.capsule, s[0].visible));
    }

    #[test]
    fn small_field_below_its_bottom_edge() {
        let s = screens();
        let f = Rect::new(200.0, 100.0, 600.0, 40.0);
        let p = place(Anchor::Field(f), &s, W, H).unwrap();
        assert_eq!(p.side, Side::Below);
        assert_eq!(p.capsule.max_y(), ax_to_cocoa(f, 982.0).y - GAP);
        assert_eq!(p.capsule.mid_x(), 500.0);
    }

    #[test]
    fn chat_box_at_window_bottom_puts_pill_above() {
        let s = screens();
        let f = Rect::new(200.0, 860.0, 600.0, 40.0); // Cocoa y 82..122, just above the dock
        let p = place(Anchor::Field(f), &s, W, H).unwrap();
        assert_eq!(p.side, Side::Above);
        assert_eq!(p.capsule.y, ax_to_cocoa(f, 982.0).max_y() + GAP);
    }

    #[test]
    fn tall_field_uses_bottom_of_visible_portion() {
        let s = screens();
        // Editor from AX y 50 down past the bottom of the screen (partly off-screen).
        let f = Rect::new(100.0, 50.0, 800.0, 1200.0);
        let p = place(Anchor::Field(f), &s, W, H).unwrap();
        assert_eq!(p.side, Side::InsideBottom);
        assert_eq!(p.capsule.y, s[0].visible.y + BOTTOM_GAP);
        assert_eq!(p.capsule.mid_x(), 500.0);
        assert!(inside(p.capsule, s[0].visible));
    }

    #[test]
    fn terminal_like_field_goes_inside() {
        let s = screens();
        // Ghostty: text area fills the window.
        let win = Rect::new(100.0, 100.0, 800.0, 500.0);
        let field = Rect::new(100.0, 130.0, 800.0, 470.0);
        let a = field_anchor(field, Some(win), true).unwrap();
        assert_eq!(a, Anchor::LargeField(field));
        // Large but not editable (web area, AXGroup): still inside.
        assert_eq!(field_anchor(field, Some(win), false), Some(Anchor::LargeField(field)));
        assert_eq!(a.strategy(), "field");
        let p = place(a, &s, W, H).unwrap();
        assert_eq!(p.side, Side::InsideBottom);
        assert_eq!(p.capsule.y, 382.0 + BOTTOM_GAP);
        // A one-line field in the same window stays a normal field.
        let small = Rect::new(120.0, 140.0, 300.0, 24.0);
        assert_eq!(field_anchor(small, Some(win), true), Some(Anchor::Field(small)));
        assert!(matches!(field_anchor(field, None, true), Some(Anchor::Field(_))));
        // A focused heading/button is not where the user types: fall through to the window.
        assert_eq!(field_anchor(small, Some(win), false), None);
        assert_eq!(caret_from_marker(Rect::new(10.0, 20.0, 0.0, 15.0)), Rect::new(10.0, 20.0, 0.0, 15.0));
        assert_eq!(caret_from_marker(Rect::new(10.0, 20.0, 30.0, 15.0)).x, 40.0);
    }

    #[test]
    fn window_bottom_center_inside() {
        let s = screens();
        let win = Rect::new(100.0, 100.0, 800.0, 500.0); // Cocoa y 382..882
        let p = place(Anchor::Window(win), &s, W, H).unwrap();
        assert_eq!(p.side, Side::InsideBottom);
        assert_eq!(p.capsule.y, 382.0 + BOTTOM_GAP);
        assert_eq!(p.capsule.mid_x(), 500.0);
    }

    #[test]
    fn mouse_fallback_is_bottom_center_of_visible_frame() {
        let s = screens();
        let p = place(Anchor::Mouse(2000.0, 500.0), &s, W, H).unwrap();
        assert_eq!(p.screen, 1);
        assert_eq!(p.capsule.mid_x(), 1512.0 + 960.0);
        assert_eq!(p.capsule.y, 0.0 + BOTTOM_GAP);
    }

    #[test]
    fn multi_screen_picks_screen_containing_anchor() {
        let s = screens();
        // AX y 500 on the secondary (primary_h 982): Cocoa y = 982-518 = 464, x 2500 -> screen 1.
        let p = place(Anchor::Caret(Rect::new(2500.0, 500.0, 0.0, 18.0)), &s, W, H).unwrap();
        assert_eq!(p.screen, 1);
        assert!(inside(p.capsule, s[1].visible));
        // A caret straddling the right edge of the secondary is clamped into it.
        let p = place(Anchor::Caret(Rect::new(3430.0, 500.0, 0.0, 18.0)), &s, W, H).unwrap();
        assert_eq!(p.capsule.max_x(), 3432.0);
    }

    #[test]
    fn screen_for_point_nearest_when_outside() {
        let s = screens();
        assert_eq!(screen_for_point(&s, 100.0, 100.0), Some(0));
        assert_eq!(screen_for_point(&s, 1600.0, 1070.0), Some(1)); // only the secondary is that tall
        assert_eq!(screen_for_point(&s, 5000.0, 10.0), Some(1));
        assert_eq!(screen_for_point(&s, -50.0, 10.0), Some(0));
        assert_eq!(screen_for_point(&[], 0.0, 0.0), None);
        assert!(place(Anchor::Mouse(0.0, 0.0), &[], W, H).is_none());
    }

    #[test]
    fn caret_validation() {
        let s = screens();
        assert!(valid_caret_ax(Rect::new(400.0, 300.0, 0.0, 18.0), &s));
        assert!(!valid_caret_ax(Rect::new(0.0, 0.0, 0.0, 0.0), &s), "zero rect");
        assert!(!valid_caret_ax(Rect::new(0.0, 0.0, 0.0, 18.0), &s), "chromium failure rect");
        assert!(!valid_caret_ax(Rect::new(400.0, 300.0, 5.0, 0.0), &s), "zero height");
        assert!(!valid_caret_ax(Rect::new(400.0, 300.0, 0.0, 900.0), &s), "too tall for a caret");
        assert!(!valid_caret_ax(Rect::new(-9000.0, 300.0, 0.0, 18.0), &s), "off every screen");
        assert!(!valid_caret_ax(Rect::new(f64::NAN, 300.0, 0.0, 18.0), &s));
        assert!(valid_frame_ax(Rect::new(10.0, 10.0, 300.0, 30.0), &s));
        assert!(!valid_frame_ax(Rect::new(10.0, 10.0, 0.0, 30.0), &s));
        assert!(!valid_frame_ax(Rect::new(-9000.0, 10.0, 300.0, 30.0), &s));
    }

    /// Values measured in TextEdit (AX coords): field at y -905, 13 pt lines, zero-length rects
    /// one line too high.
    #[test]
    fn caret_from_character_rects() {
        let after = Rect::new(-1255.9, -905.0, 6.6, 13.0);
        assert_eq!(caret_from_char_after(after), Rect::new(-1255.9, -905.0, 0.0, 13.0));
        let before = Rect::new(-1077.0, -879.0, 6.5, 13.0);
        assert_eq!(caret_from_char_before(before, false, None), Rect::new(-1070.5, -879.0, 0.0, 13.0));
        // End of text right after "\n": caret is at the start of the next (empty) line.
        let nl = Rect::new(-1070.5, -879.0, 417.5, 13.0);
        let len0 = Rect::new(-1289.0, -879.0, 0.0, 13.0);
        assert_eq!(caret_from_char_before(nl, true, Some(len0)), Rect::new(-1289.0, -866.0, 0.0, 13.0));
        assert_eq!(caret_from_char_before(nl, true, None).y, -866.0);
    }

    #[test]
    fn len0_above_field_moves_down_one_line() {
        let field = Rect::new(-1299.0, -905.0, 656.0, 384.0);
        // Empty document: TextEdit reports y -918 for the first line at -905.
        assert_eq!(fix_len0(Rect::new(-1289.0, -918.0, 0.0, 13.0), Some(field)).y, -905.0);
        // Inside the field: left alone.
        assert_eq!(fix_len0(Rect::new(-1289.0, -892.0, 0.0, 13.0), Some(field)).y, -892.0);
        // Far above the field (some other element's rect): left alone (validation decides).
        assert_eq!(fix_len0(Rect::new(0.0, -2000.0, 0.0, 13.0), Some(field)).y, -2000.0);
        assert_eq!(fix_len0(Rect::new(-1289.0, -918.0, 0.0, 13.0), None).y, -918.0);
    }

    #[test]
    fn neither_side_fits_clamps_inside() {
        // Tiny visible frame: 50 pt tall, caret in the middle.
        let s = [ScreenGeom { frame: Rect::new(0.0, 0.0, 800.0, 60.0), visible: Rect::new(0.0, 5.0, 800.0, 50.0) }];
        let p = place(Anchor::Caret(Rect::new(300.0, 20.0, 0.0, 18.0)), &s, W, H).unwrap();
        assert_eq!(p.side, Side::Clamped);
        assert!(inside(p.capsule, s[0].visible));
    }
}
