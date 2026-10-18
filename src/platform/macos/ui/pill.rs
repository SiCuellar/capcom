//! Floating "pill" overlay (Wispr Flow bar style): a small near-black capsule with white vertical
//! bars that follow the voice.
//!
//! - Position (`[ui] pill_position`): `caret` (default) anchors it just below the text insertion
//!   point of the focused app, found via the Accessibility API when the pill appears
//!   ([`anchor`] + pure geometry in [`placement`]), falling back to the focused field, the focused
//!   window, then the bottom-center of the screen under the mouse (`bottom`). The position is
//!   fixed for the rest of that dictation (it does not chase the caret).
//! - Non-activating, borderless `NSPanel`: never becomes key/main, never steals focus from the
//!   app being dictated into. Shown with `orderFrontRegardless`, never `makeKeyAndOrderFront`.
//! - Floats above everything at status-window level, on all Spaces and over full-screen apps.
//! - Ignores mouse events (click-through) for the MVP. TODO: draggable + right-click menu.
//! - Rendering: plain CALayers (capsule + [`anim::BARS`] bar layers). A 60 fps main-run-loop
//!   `NSTimer` drives [`anim::Animator`] only while the pill is visible; it is invalidated as soon
//!   as the pill is hidden, so an idle pill costs zero CPU.
//!
//! Must be created and driven on the AppKit main thread ([`UiComponent::handle`] contract).

pub mod anchor;
pub mod anim;
pub mod demo;
pub mod placement;

use crate::config::PillPosition;
use crate::ui::{UiComponent, UiEvent};
use placement::{Anchor, Placement, ScreenGeom};
use anim::{Animator, Frame, BARS, BAR_GAP, BAR_MIN_H, BAR_W};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::{MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSPanel, NSScreen, NSStatusWindowLevel, NSView, NSWindowAnimationBehavior,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_core_foundation::{CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGPath};
use objc2_foundation::{NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSTimer};
use objc2_quartz_core::{CALayer, CATransaction, CATransform3D};
use std::cell::RefCell;
use std::ptr::NonNull;
use std::rc::{Rc, Weak};
use std::time::Instant;

/// Visible capsule size (points).
pub const PILL_W: f64 = 112.0;
pub const PILL_H: f64 = 32.0;
/// Transparent margin around the capsule so its shadow is not clipped by the window.
const MARGIN: f64 = 18.0;
const FPS: f64 = 60.0;

/// Near-black capsule fill (r, g, b, a) and the error red it tints toward.
const FILL: [f64; 4] = [0.055, 0.055, 0.063, 0.94];
const FILL_ERR: [f64; 4] = [0.74, 0.15, 0.16, 0.96];

struct Inner {
    mtm: MainThreadMarker,
    panel: Retained<NSPanel>,
    capsule: Retained<CALayer>,
    bars: Vec<Retained<CALayer>>,
    timer: Option<Retained<NSTimer>>,
    anim: Animator,
    last: Instant,
    last_tint: f32,
    on_screen: bool,
    position: PillPosition,
    /// Human-readable description of the last placement (strategy, rects, timing).
    last_placement: String,
    weak: Weak<RefCell<Inner>>,
}

/// The pill overlay. Register it with the app host as a [`UiComponent`].
pub struct Pill {
    inner: Rc<RefCell<Inner>>,
}

fn cg(c: [f64; 4]) -> objc2_core_foundation::CFRetained<CGColor> {
    CGColor::new_generic_rgb(c[0], c[1], c[2], c[3])
}

fn lerp4(a: [f64; 4], b: [f64; 4], t: f64) -> [f64; 4] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t, a[3] + (b[3] - a[3]) * t]
}

impl Pill {
    pub fn new(mtm: MainThreadMarker, position: PillPosition) -> Self {
        let win_w = PILL_W + 2.0 * MARGIN;
        let win_h = PILL_H + 2.0 * MARGIN;
        let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(win_w, win_h));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe { panel.setReleasedWhenClosed(false) };
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setHidesOnDeactivate(false);
        panel.setWorksWhenModal(true);
        panel.setLevel(NSStatusWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        panel.setIgnoresMouseEvents(true);
        panel.setOpaque(false);
        panel.setHasShadow(false); // the capsule layer draws its own shadow
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setAnimationBehavior(NSWindowAnimationBehavior::None);
        panel.setAlphaValue(0.0);

        let view = NSView::initWithFrame(NSView::alloc(mtm), rect);
        view.setWantsLayer(true);
        panel.setContentView(Some(&view));
        let root = view.layer().expect("layer-backed view has a layer");

        let scale = NSScreen::mainScreen(mtm).map(|s| s.backingScaleFactor()).unwrap_or(2.0);
        let capsule = CALayer::layer();
        capsule.setContentsScale(scale);
        capsule.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(PILL_W, PILL_H)));
        capsule.setPosition(CGPoint::new(win_w / 2.0, win_h / 2.0));
        capsule.setCornerRadius(PILL_H / 2.0);
        capsule.setBackgroundColor(Some(&cg(FILL)));
        capsule.setBorderWidth(1.0);
        capsule.setBorderColor(Some(&cg([1.0, 1.0, 1.0, 0.13])));
        capsule.setShadowColor(Some(&cg([0.0, 0.0, 0.0, 1.0])));
        capsule.setShadowOpacity(0.45);
        capsule.setShadowRadius(8.0);
        capsule.setShadowOffset(CGSize::new(0.0, -2.0));
        let sp = unsafe {
            CGPath::with_rounded_rect(
                CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(PILL_W, PILL_H)),
                PILL_H / 2.0,
                PILL_H / 2.0,
                std::ptr::null(),
            )
        };
        capsule.setShadowPath(Some(&sp));
        root.addSublayer(&capsule);

        let total = BARS as f64 * BAR_W + (BARS as f64 - 1.0) * BAR_GAP;
        let x0 = (PILL_W - total) / 2.0 + BAR_W / 2.0;
        let white = cg([1.0, 1.0, 1.0, 1.0]);
        let bars = (0..BARS)
            .map(|i| {
                let b = CALayer::layer();
                b.setContentsScale(scale);
                b.setBackgroundColor(Some(&white));
                b.setCornerRadius(BAR_W / 2.0);
                b.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(BAR_W, BAR_MIN_H)));
                b.setPosition(CGPoint::new(x0 + i as f64 * (BAR_W + BAR_GAP), PILL_H / 2.0));
                capsule.addSublayer(&b);
                b
            })
            .collect();

        let inner = Rc::new_cyclic(|weak| {
            RefCell::new(Inner {
                mtm,
                panel,
                capsule,
                bars,
                timer: None,
                anim: Animator::new(),
                last: Instant::now(),
                last_tint: 0.0,
                on_screen: false,
                position,
                last_placement: String::new(),
                weak: weak.clone(),
            })
        });
        Self { inner }
    }

    /// True while the pill is on screen (including fade-out).
    pub fn is_visible(&self) -> bool {
        self.inner.borrow().on_screen
    }

    /// Description of where the pill was last placed and why (strategy, AX rects, timing).
    pub fn last_placement(&self) -> String {
        self.inner.borrow().last_placement.clone()
    }

    /// Debug aid: render the pill's current frame (layers + window alpha) to a PNG at 3x, twice
    /// side by side over a light and a dark backdrop. Works without Screen Recording permission
    /// (used by `capcom pill-demo` with `CAPCOM_PILL_SNAPSHOTS=<dir>`).
    pub fn snapshot_png(&self, path: &std::path::Path) -> anyhow::Result<()> {
        use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep};
        use objc2_core_graphics::{CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImageAlphaInfo};
        use objc2::AllocAnyThread;
        use objc2_foundation::NSDictionary;
        let inner = self.inner.borrow();
        let Some(view) = inner.panel.contentView() else { anyhow::bail!("no content view") };
        let Some(root) = view.layer() else { anyhow::bail!("no root layer") };
        let (w, h, k) = (PILL_W + 2.0 * MARGIN, PILL_H + 2.0 * MARGIN, 3.0);
        let (pw, ph) = ((2.0 * w * k) as usize, (h * k) as usize);
        let cs = CGColorSpace::new_device_rgb();
        let ctx = unsafe {
            CGBitmapContextCreate(std::ptr::null_mut(), pw, ph, 8, 0, cs.as_deref(), CGImageAlphaInfo::PremultipliedLast.0)
        }
        .ok_or_else(|| anyhow::anyhow!("CGBitmapContextCreate failed"))?;
        let c = Some(&*ctx);
        CGContext::scale_ctm(c, k, k);
        CGContext::set_rgb_fill_color(c, 0.93, 0.93, 0.94, 1.0);
        CGContext::fill_rect(c, CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h)));
        CGContext::set_rgb_fill_color(c, 0.16, 0.18, 0.22, 1.0);
        CGContext::fill_rect(c, CGRect::new(CGPoint::new(w, 0.0), CGSize::new(w, h)));
        CGContext::set_alpha(c, inner.panel.alphaValue());
        root.renderInContext(&ctx);
        CGContext::translate_ctm(c, w, 0.0);
        root.renderInContext(&ctx);
        let img = CGBitmapContextCreateImage(c).ok_or_else(|| anyhow::anyhow!("CGBitmapContextCreateImage failed"))?;
        let rep = NSBitmapImageRep::initWithCGImage(NSBitmapImageRep::alloc(), &img);
        let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }
            .ok_or_else(|| anyhow::anyhow!("PNG encoding failed"))?;
        std::fs::write(path, data.to_vec())?;
        Ok(())
    }
}

impl UiComponent for Pill {
    fn handle(&mut self, ev: &UiEvent) {
        if let UiEvent::Error(msg) = ev {
            tracing::debug!("pill error flash: {msg}");
        }
        let mut inner = self.inner.borrow_mut();
        if inner.anim.event(ev) {
            inner.ensure_shown();
        }
    }
}

impl Inner {
    fn ensure_shown(&mut self) {
        if !self.on_screen {
            self.place();
            self.last = Instant::now();
            let f = self.anim.tick(0.0);
            self.apply(&f);
            self.panel.orderFrontRegardless();
            self.on_screen = true;
        }
        if self.timer.is_none() {
            let weak = self.weak.clone();
            let block = RcBlock::new(move |_t: NonNull<NSTimer>| {
                if let Some(rc) = weak.upgrade() {
                    // `try_borrow_mut`: never re-enter if a handler is mid-update.
                    if let Ok(mut inner) = rc.try_borrow_mut() {
                        inner.frame();
                    }
                }
            });
            // SAFETY: the timer is added to the *main* run loop and only ever fires on the main
            // thread, the same thread that owns the Rc captured by the block.
            let timer = unsafe { NSTimer::timerWithTimeInterval_repeats_block(1.0 / FPS, true, &block) };
            unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
            self.timer = Some(timer);
        }
    }

    /// Anchor to the caret / focused field / window (or the screen under the mouse), once per show.
    fn place(&mut self) {
        let (screens, geoms) = screen_geoms(self.mtm);
        let (p, desc) = compute_placement(&geoms, self.position, None);
        let Some(p) = p else { return };
        self.panel.setFrameOrigin(window_origin(&p));
        tracing::debug!("pill placed: {desc}");
        self.last_placement = desc;
        if let Some(screen) = screens.get(p.screen) {
            let scale = screen.backingScaleFactor();
            self.capsule.setContentsScale(scale);
            for b in &self.bars {
                b.setContentsScale(scale);
            }
        }
    }

    fn frame(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32();
        self.last = now;
        let f = self.anim.tick(dt);
        self.apply(&f);
        if !f.visible {
            self.panel.orderOut(None);
            self.on_screen = false;
            if let Some(t) = self.timer.take() {
                t.invalidate();
            }
        }
    }

    fn apply(&mut self, f: &Frame) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        for ((b, &h), &o) in self.bars.iter().zip(f.heights.iter()).zip(f.opacity.iter()) {
            b.setBounds(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(BAR_W, h as CGFloat)));
            b.setOpacity(o);
        }
        let s = f.scale as CGFloat;
        self.capsule.setTransform(CATransform3D::new_scale(s, s, 1.0));
        if (f.tint - self.last_tint).abs() > 1e-3 {
            self.capsule.setBackgroundColor(Some(&cg(lerp4(FILL, FILL_ERR, f.tint as f64))));
            self.last_tint = f.tint;
        }
        CATransaction::commit();
        self.panel.setAlphaValue(f.alpha as CGFloat);
    }
}

/// `NSScreen.screens` (primary first) and their geometry.
pub fn screen_geoms(mtm: MainThreadMarker) -> (Vec<Retained<NSScreen>>, Vec<ScreenGeom>) {
    let screens: Vec<_> = NSScreen::screens(mtm).iter().collect();
    let r = |f: NSRect| placement::Rect::new(f.origin.x, f.origin.y, f.size.width, f.size.height);
    let geoms = screens.iter().map(|s| ScreenGeom { frame: r(s.frame()), visible: r(s.visibleFrame()) }).collect();
    (screens, geoms)
}

/// Run the anchor lookup (if `position` is `caret`) and the placement geometry.
/// Returns the placement and a one-line description for logs / `anchor-probe`.
/// `target`: probe this pid instead of the focused app (debugging).
pub fn compute_placement(geoms: &[ScreenGeom], position: PillPosition, target: Option<i32>) -> (Option<Placement>, String) {
    let mouse = NSEvent::mouseLocation();
    let mut desc = String::new();
    let anchor = match position {
        PillPosition::Bottom => {
            desc.push_str("strategy=screen (pill_position=bottom)");
            None
        }
        PillPosition::Caret => {
            let rep = anchor::find_anchor_for(geoms, target);
            desc = rep.to_string();
            rep.anchor
        }
    };
    let mut p = anchor.and_then(|a| placement::place(a, geoms, PILL_W, PILL_H));
    if p.is_none() {
        p = placement::place(Anchor::Mouse(mouse.x, mouse.y), geoms, PILL_W, PILL_H);
    }
    if let Some(p) = &p {
        let c = p.capsule;
        desc.push_str(&format!(
            " -> pill[cocoa]=({:.0},{:.0} {:.0}x{:.0}) {:?} on screen {}",
            c.x, c.y, c.w, c.h, p.side, p.screen
        ));
    }
    (p, desc)
}

/// Panel origin for a capsule placement (the window has a shadow margin around the capsule).
fn window_origin(p: &Placement) -> NSPoint {
    NSPoint::new((p.capsule.x - MARGIN).round(), (p.capsule.y - MARGIN).round())
}

impl Drop for Inner {
    fn drop(&mut self) {
        if let Some(t) = self.timer.take() {
            t.invalidate();
        }
        self.panel.orderOut(None);
    }
}
