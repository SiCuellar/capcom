//! Find where the user is typing via the Accessibility API, to anchor the pill there.
//!
//! Strategy (first that yields a plausible rect wins, see [`find_anchor`]):
//! 1. **caret**: focused element -> `AXSelectedTextRange` -> `AXBoundsForRange` (length 0 at the
//!    insertion point; else the character before it; else the character after it).
//! 2. **field**: focused element `AXPosition` + `AXSize`.
//! 3. **window**: focused application's `AXFocusedWindow` frame.
//! 4. **screen**: none of the above; caller falls back to the screen under the mouse.
//!
//! Chromium / Electron apps (Chrome, Slack, Discord, VS Code, Notion, ...) build no AX tree, or
//! only a basic one without text geometry (0x0 range bounds), until an assistive client asks.
//! When the focused app yields no caret, we set `AXManualAccessibility = true` on the application
//! element (Electron's documented switch; no side effects besides the tree's memory/CPU cost;
//! other apps just reject it). Only if that did not produce a caret, and only for known Chromium
//! *browsers*, do we also set `AXEnhancedUserInterface = true` (what Chrome listens to): that flag
//! is what VoiceOver sets, and on some apps it disables/janks window animations and confuses
//! window managers (Rectangle, Magnet), so it is never set on arbitrary apps. Each pid is tried
//! once per process lifetime (the flags stay set for the target app's lifetime).
//!
//! Every AX message gets a timeout of min([`AX_TIMEOUT_S`], time left in [`BUDGET`]) and no
//! message is sent after the budget is spent, so a hung app can never stall the main thread for
//! much more than [`BUDGET`].
//! Main thread only (called from the pill's show path).

use super::placement::{
    caret_from_char_after, caret_from_char_before, caret_from_marker, field_anchor, fix_len0, valid_caret_ax, valid_frame_ax, Anchor, Rect, ScreenGeom,
};
use objc2_app_kit::{NSRunningApplication, NSWorkspace};
use objc2_core_foundation::CFString;
use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::c_void;
use std::fmt::Write as _;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

/// Per-message AX timeout (seconds), applied globally via the system-wide element.
pub const AX_TIMEOUT_S: f32 = 0.12;
/// Total lookup budget: no new AX step starts after this.
pub const BUDGET: Duration = Duration::from_millis(150);
/// After enabling the AX tree in a Chromium/Electron app, poll this often for it to appear.
const ENABLE_POLL: Duration = Duration::from_millis(25);

type CFTypeRef = *const c_void;
type AXError = i32;
const AX_OK: AXError = 0;

const AX_VALUE_CGPOINT: u32 = 1;
const AX_VALUE_CGSIZE: u32 = 2;
const AX_VALUE_CGRECT: u32 = 3;
const AX_VALUE_CFRANGE: u32 = 4;

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct P {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct S {
    w: f64,
    h: f64,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct R {
    o: P,
    s: S,
}
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct CFRange {
    location: isize,
    length: isize,
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> CFTypeRef;
    fn AXUIElementCreateApplication(pid: i32) -> CFTypeRef;
    fn AXUIElementCopyAttributeValue(el: CFTypeRef, attr: CFTypeRef, out: *mut CFTypeRef) -> AXError;
    fn AXUIElementCopyParameterizedAttributeValue(
        el: CFTypeRef,
        attr: CFTypeRef,
        param: CFTypeRef,
        out: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(el: CFTypeRef, attr: CFTypeRef, value: CFTypeRef) -> AXError;
    fn AXUIElementSetMessagingTimeout(el: CFTypeRef, seconds: f32) -> AXError;
    fn AXUIElementGetPid(el: CFTypeRef, pid: *mut i32) -> AXError;
    fn AXValueCreate(ty: u32, ptr: *const c_void) -> CFTypeRef;
    fn AXValueGetValue(v: CFTypeRef, ty: u32, out: *mut c_void) -> bool;
    fn AXValueGetTypeID() -> usize;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: CFTypeRef);
    fn CFGetTypeID(cf: CFTypeRef) -> usize;
    fn CFStringGetTypeID() -> usize;
    static kCFBooleanTrue: CFTypeRef;
}

/// Owned (+1) CoreFoundation reference.
struct Cf(NonNull<c_void>);

impl Cf {
    fn from_create(p: CFTypeRef) -> Option<Self> {
        NonNull::new(p as *mut c_void).map(Cf)
    }
    fn ptr(&self) -> CFTypeRef {
        self.0.as_ptr()
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.ptr()) }
    }
}

fn cfstr(s: &'static str) -> objc2_core_foundation::CFRetained<CFString> {
    CFString::from_static_str(s)
}

fn sref(s: &CFString) -> CFTypeRef {
    s as *const CFString as CFTypeRef
}

/// kAXErrorCannotComplete: also returned when the lookup deadline has passed.
const AX_CANNOT_COMPLETE: AXError = -25204;

thread_local! {
    /// Hard deadline of the lookup in progress (see [`arm`]).
    static DEADLINE: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
}

/// Before each AX message: cap `el`'s messaging timeout at what is left of the budget, or fail
/// fast if nothing is left. A local call (no IPC), so it is cheap.
fn arm(el: &Cf) -> Result<(), AXError> {
    let Some(deadline) = DEADLINE.with(|d| d.get()) else { return Ok(()) };
    let left = deadline.saturating_duration_since(Instant::now()).as_secs_f32();
    if left < 0.005 {
        return Err(AX_CANNOT_COMPLETE);
    }
    unsafe { AXUIElementSetMessagingTimeout(el.ptr(), left.min(AX_TIMEOUT_S)) };
    Ok(())
}

fn copy_attr(el: &Cf, attr: &'static str) -> Result<Cf, AXError> {
    arm(el)?;
    let a = cfstr(attr);
    let mut out: CFTypeRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(el.ptr(), sref(&a), &mut out) };
    match (err, Cf::from_create(out)) {
        (AX_OK, Some(v)) => Ok(v),
        (AX_OK, None) => Err(-25212), // kAXErrorNoValue
        (e, _) => Err(e),
    }
}

fn copy_param(el: &Cf, attr: &'static str, param: &Cf) -> Result<Cf, AXError> {
    arm(el)?;
    let a = cfstr(attr);
    let mut out: CFTypeRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyParameterizedAttributeValue(el.ptr(), sref(&a), param.ptr(), &mut out) };
    match (err, Cf::from_create(out)) {
        (AX_OK, Some(v)) => Ok(v),
        (AX_OK, None) => Err(-25212),
        (e, _) => Err(e),
    }
}

fn set_true(el: &Cf, attr: &'static str) -> AXError {
    if let Err(e) = arm(el) {
        return e;
    }
    let a = cfstr(attr);
    unsafe { AXUIElementSetAttributeValue(el.ptr(), sref(&a), kCFBooleanTrue) }
}

fn ax_value<T: Default>(v: &Cf, ty: u32) -> Option<T> {
    if unsafe { CFGetTypeID(v.ptr()) } != unsafe { AXValueGetTypeID() } {
        return None;
    }
    let mut out = T::default();
    unsafe { AXValueGetValue(v.ptr(), ty, &mut out as *mut T as *mut c_void) }.then_some(out)
}

fn string_attr(el: &Cf, attr: &'static str) -> Option<String> {
    let v = copy_attr(el, attr).ok()?;
    if unsafe { CFGetTypeID(v.ptr()) } != unsafe { CFStringGetTypeID() } {
        return None;
    }
    // SAFETY: type-checked above; `v` keeps it alive for the duration of the borrow.
    let s: &CFString = unsafe { &*(v.ptr() as *const CFString) };
    Some(s.to_string())
}

fn frame_of(el: &Cf) -> Result<Rect, AXError> {
    let p: P = ax_value(&copy_attr(el, "AXPosition")?, AX_VALUE_CGPOINT).ok_or(-25212)?;
    let s: S = ax_value(&copy_attr(el, "AXSize")?, AX_VALUE_CGSIZE).ok_or(-25212)?;
    Ok(Rect::new(p.x, p.y, s.w, s.h))
}

fn bounds_for(el: &Cf, loc: isize, len: isize) -> Option<Rect> {
    let range = CFRange { location: loc, length: len };
    let param = Cf::from_create(unsafe { AXValueCreate(AX_VALUE_CFRANGE, &range as *const CFRange as *const c_void) })?;
    let v = copy_param(el, "AXBoundsForRange", &param).ok()?;
    let r: R = ax_value(&v, AX_VALUE_CGRECT)?;
    Some(Rect::new(r.o.x, r.o.y, r.s.w, r.s.h))
}

fn string_for(el: &Cf, loc: isize, len: isize) -> Option<String> {
    let range = CFRange { location: loc, length: len };
    let param = Cf::from_create(unsafe { AXValueCreate(AX_VALUE_CFRANGE, &range as *const CFRange as *const c_void) })?;
    let v = copy_param(el, "AXStringForRange", &param).ok()?;
    if unsafe { CFGetTypeID(v.ptr()) } != unsafe { CFStringGetTypeID() } {
        return None;
    }
    // SAFETY: type-checked above; `v` outlives the borrow.
    Some(unsafe { &*(v.ptr() as *const CFString) }.to_string())
}

/// Everything the lookup saw; `Display` is the `anchor-probe` / debug-log line.
#[derive(Debug, Clone)]
pub struct AnchorReport {
    pub app: String,
    pub bundle_id: String,
    pub pid: i32,
    pub trusted: bool,
    pub role: Option<String>,
    /// Raw `AXSelectedTextRange` (location, length).
    pub selection: Option<(isize, isize)>,
    /// Caret rect (AX coords) and which range produced it.
    pub caret: Option<(Rect, &'static str)>,
    pub field: Option<Rect>,
    pub window: Option<Rect>,
    /// `AXManualAccessibility` / `AXEnhancedUserInterface` set during this lookup.
    pub enabled: Vec<&'static str>,
    pub notes: Vec<String>,
    pub elapsed: Duration,
    /// `None` = fall back to the screen under the mouse.
    pub anchor: Option<Anchor>,
}

impl AnchorReport {
    pub fn strategy(&self) -> &'static str {
        self.anchor.map(|a| a.strategy()).unwrap_or("screen")
    }
}

fn fmt_rect(r: &Rect) -> String {
    format!("({:.0},{:.0} {:.0}x{:.0})", r.x, r.y, r.w, r.h)
}

impl std::fmt::Display for AnchorReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = format!(
            "strategy={} app=\"{}\" ({}, pid {}) role={} in {:.1} ms",
            self.strategy(),
            self.app,
            self.bundle_id,
            self.pid,
            self.role.as_deref().unwrap_or("-"),
            self.elapsed.as_secs_f64() * 1e3
        );
        if let Some((l, n)) = self.selection {
            let _ = write!(s, " sel=({l},{n})");
        }
        if let Some((r, how)) = &self.caret {
            let _ = write!(s, " caret[ax]={} via {how}", fmt_rect(r));
        }
        if let Some(r) = &self.field {
            let _ = write!(s, " field[ax]={}", fmt_rect(r));
        }
        if let Some(r) = &self.window {
            let _ = write!(s, " window[ax]={}", fmt_rect(r));
        }
        if !self.trusted {
            s.push_str(" (process NOT Accessibility-trusted)");
        }
        if !self.enabled.is_empty() {
            let _ = write!(s, " enabled={:?}", self.enabled);
        }
        if !self.notes.is_empty() {
            let _ = write!(s, " notes=[{}]", self.notes.join("; "));
        }
        f.write_str(&s)
    }
}

thread_local! {
    /// Pids we already tried to switch the AX tree on for (once per pid).
    static ENABLED_PIDS: RefCell<HashSet<i32>> = RefCell::new(HashSet::new());
    static SYSTEM_WIDE: RefCell<Option<Cf>> = const { RefCell::new(None) };
}

/// Bundle-id prefixes of Chromium browsers that need `AXEnhancedUserInterface` (they ignore
/// Electron's `AXManualAccessibility`).
const CHROMIUM_BROWSERS: &[&str] = &[
    "com.google.Chrome",
    "org.chromium.Chromium",
    "com.brave.Browser",
    "com.microsoft.edgemac",
    "com.vivaldi.Vivaldi",
    "company.thebrowser.Browser",
    "com.operasoftware.Opera",
];

/// Roles whose frame is a sensible "where I'm typing" anchor when there is no caret.
const EDITABLE_ROLES: &[&str] = &["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField", "AXSecureTextField"];

fn with_system_wide<T>(f: impl FnOnce(&Cf) -> T) -> Option<T> {
    SYSTEM_WIDE.with(|sw| {
        let mut sw = sw.borrow_mut();
        if sw.is_none() {
            let el = Cf::from_create(unsafe { AXUIElementCreateSystemWide() })?;
            // On the system-wide element this sets the default timeout for every element.
            unsafe { AXUIElementSetMessagingTimeout(el.ptr(), AX_TIMEOUT_S) };
            *sw = Some(el);
        }
        sw.as_ref().map(f)
    })
}

/// Caret / field info from the focused element.
struct Focus {
    role: Option<String>,
    selection: Option<(isize, isize)>,
    caret: Option<(Rect, &'static str)>,
    field: Option<Rect>,
}

fn read_focus(app_el: &Cf, screens: &[ScreenGeom], t0: Instant, notes: &mut Vec<String>) -> Result<Focus, AXError> {
    let el = copy_attr(app_el, "AXFocusedUIElement")?;
    let mut f = Focus { role: string_attr(&el, "AXRole"), selection: None, caret: None, field: None };
    // Field first: the caret fallbacks use it for sanity checks.
    match frame_of(&el) {
        Ok(r) if valid_frame_ax(r, screens) => f.field = Some(r),
        Ok(r) => notes.push(format!("field rejected {}", fmt_rect(&r))),
        Err(e) => notes.push(format!("field: AXError {e}")),
    }
    let Some(rng) = copy_attr(&el, "AXSelectedTextRange").ok().and_then(|v| ax_value::<CFRange>(&v, AX_VALUE_CFRANGE))
    else {
        return Ok(f);
    };
    f.selection = Some((rng.location, rng.length));
    // Insertion point (with a selection: its end, where the user's eye is).
    let loc = (rng.location + rng.length.max(0)).max(0);
    let mut accept = |r: Rect, how: &'static str, notes: &mut Vec<String>| {
        if valid_caret_ax(r, screens) {
            f.caret = Some((r, how));
            true
        } else {
            notes.push(format!("{how} rejected {}", fmt_rect(&r)));
            false
        }
    };
    // Character boxes are reliable everywhere; zero-length ranges are not (AppKit's NSTextView
    // reports them one line too high), so they are the last resort.
    if t0.elapsed() < BUDGET {
        if let Some(r) = bounds_for(&el, loc, 1) {
            if accept(caret_from_char_after(r), "char-after", notes) {
                return Ok(f);
            }
        }
    }
    let len0 = if t0.elapsed() < BUDGET { bounds_for(&el, loc, 0) } else { None };
    if loc > 0 && t0.elapsed() < BUDGET {
        if let Some(r) = bounds_for(&el, loc - 1, 1) {
            let newline = string_for(&el, loc - 1, 1).is_some_and(|c| c == "\n" || c == "\r" || c == "\u{2028}");
            let how = if newline { "char-before-newline" } else { "char-before" };
            if accept(caret_from_char_before(r, newline, len0), how, notes) {
                return Ok(f);
            }
        }
    }
    // WebKit / Chromium: text markers carry real geometry even when Chromium answers range
    // queries with a 0x0 rect at the web-area origin (no inline text boxes in its basic AX mode).
    if t0.elapsed() < BUDGET {
        if let Some(r) = copy_attr(&el, "AXSelectedTextMarkerRange")
            .ok()
            .and_then(|m| copy_param(&el, "AXBoundsForTextMarkerRange", &m).ok())
            .and_then(|v| ax_value::<R>(&v, AX_VALUE_CGRECT))
        {
            let r = Rect::new(r.o.x, r.o.y, r.s.w, r.s.h);
            if accept(caret_from_marker(r), "text-marker", notes) {
                return Ok(f);
            }
        }
    }
    match len0 {
        Some(r) => {
            accept(fix_len0(r, f.field), "len0", notes);
        }
        None => notes.push("no bounds for the insertion point".into()),
    }
    Ok(f)
}

/// Pid of the focused application: system-wide `AXFocusedApplication` first (AppKit's
/// `frontmostApplication` is only refreshed by run-loop notifications and can be stale), else
/// AppKit's frontmost app. (On macOS 27 the system-wide query answers kAXErrorNoValue /
/// CannotComplete, quickly, so the fallback is what actually runs there.)
fn focused_pid(notes: &mut Vec<String>) -> Result<i32, AXError> {
    with_system_wide(|sw| {
        let app = copy_attr(sw, "AXFocusedApplication")?;
        let mut pid = 0;
        match unsafe { AXUIElementGetPid(app.ptr(), &mut pid) } {
            AX_OK => Ok(pid),
            e => Err(e),
        }
    })
    .unwrap_or(Err(-25200))
    .or_else(|e| {
        // Fallback: AppKit's idea of the frontmost app (fresh while a run loop is spinning).
        notes.push(format!("system-wide AXFocusedApplication: AXError {e}; using NSWorkspace"));
        let ws = NSWorkspace::sharedWorkspace();
        ws.frontmostApplication().map(|a| a.processIdentifier()).filter(|&p| p > 0).ok_or(e)
    })
}

/// Look up the anchor for the focused app. Never blocks much longer than [`BUDGET`].
pub fn find_anchor(screens: &[ScreenGeom]) -> AnchorReport {
    find_anchor_for(screens, None)
}

/// Like [`find_anchor`], for a specific app (`pid`) instead of the focused one (debugging:
/// `capcom anchor-probe --pid`, works on background apps without stealing focus).
pub fn find_anchor_for(screens: &[ScreenGeom], target: Option<i32>) -> AnchorReport {
    let t0 = Instant::now();
    let rep = AnchorReport {
        app: String::new(),
        bundle_id: String::new(),
        pid: 0,
        trusted: crate::platform::macos::secure::accessibility_trusted(),
        role: None,
        selection: None,
        caret: None,
        field: None,
        window: None,
        enabled: vec![],
        notes: vec![],
        elapsed: Duration::ZERO,
        anchor: None,
    };
    with_system_wide(|_| ()); // installs the global messaging timeout
    DEADLINE.with(|d| d.set(Some(t0 + BUDGET)));
    let rep = lookup(screens, target, t0, rep);
    DEADLINE.with(|d| d.set(None));
    rep
}

fn lookup(screens: &[ScreenGeom], target: Option<i32>, t0: Instant, mut rep: AnchorReport) -> AnchorReport {
    let pid = match target {
        Some(p) => Ok(p),
        None => focused_pid(&mut rep.notes),
    };
    let pid = match pid {
        Ok(p) => p,
        Err(e) => {
            rep.notes.push(format!("no AXFocusedApplication (AXError {e})"));
            rep.elapsed = t0.elapsed();
            return rep;
        }
    };
    rep.pid = pid;
    if let Some(ra) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) {
        rep.app = ra.localizedName().map(|s| s.to_string()).unwrap_or_default();
        rep.bundle_id = ra.bundleIdentifier().map(|s| s.to_string()).unwrap_or_default();
    }
    let Some(app_el) = Cf::from_create(unsafe { AXUIElementCreateApplication(pid) }) else {
        rep.elapsed = t0.elapsed();
        return rep;
    };

    let mut focus = read_focus(&app_el, screens, t0, &mut rep.notes);
    let no_caret = |f: &Result<Focus, AXError>| f.as_ref().map(|f| f.caret.is_none()).unwrap_or(true);
    if no_caret(&focus) && t0.elapsed() < BUDGET && ENABLED_PIDS.with(|s| s.borrow_mut().insert(pid)) {
        // Electron's switch first (cheap, no side effects); Chrome's only for known Chromium
        // browsers and only if the first did not help. Each attempt polls briefly because the
        // tree is built asynchronously.
        let chromium = CHROMIUM_BROWSERS.iter().any(|p| rep.bundle_id.starts_with(p));
        for attr in ["AXManualAccessibility", "AXEnhancedUserInterface"] {
            if attr == "AXEnhancedUserInterface" && !chromium {
                break;
            }
            if t0.elapsed() >= BUDGET {
                break;
            }
            let err = set_true(&app_el, attr);
            if err != AX_OK {
                rep.notes.push(format!("set {attr}: AXError {err}"));
                continue;
            }
            rep.enabled.push(attr);
            while t0.elapsed() + ENABLE_POLL < BUDGET {
                std::thread::sleep(ENABLE_POLL);
                focus = read_focus(&app_el, screens, t0, &mut rep.notes);
                if !no_caret(&focus) {
                    break;
                }
            }
            if !no_caret(&focus) {
                break;
            }
        }
    }
    match focus {
        Ok(f) => {
            rep.role = f.role;
            rep.selection = f.selection;
            rep.caret = f.caret;
            rep.field = f.field;
        }
        Err(e) => rep.notes.push(format!("focused element: AXError {e}")),
    }
    if t0.elapsed() < BUDGET {
        match copy_attr(&app_el, "AXFocusedWindow").and_then(|w| frame_of(&w)) {
            Ok(r) if valid_frame_ax(r, screens) => rep.window = Some(r),
            Ok(r) => rep.notes.push(format!("window rejected {}", fmt_rect(&r))),
            Err(e) => rep.notes.push(format!("window: AXError {e}")),
        }
    } else {
        rep.notes.push("budget exhausted".into());
    }
    // Polling after enabling a Chromium/Electron tree repeats the same notes; keep one of each.
    let mut seen = HashSet::new();
    rep.notes.retain(|n| seen.insert(n.clone()));
    rep.anchor = choose(&rep);
    rep.elapsed = t0.elapsed();
    rep
}

/// Caret, else the focused element (if editable, or if it fills most of its window), else the
/// focused window. `None`: the caller uses the screen under the mouse.
fn choose(rep: &AnchorReport) -> Option<Anchor> {
    if let Some((r, _)) = rep.caret {
        return Some(Anchor::Caret(r));
    }
    if let Some(r) = rep.field {
        let editable = rep.role.as_deref().is_some_and(|r| EDITABLE_ROLES.contains(&r));
        if let Some(a) = field_anchor(r, rep.window, editable) {
            return Some(a);
        }
    }
    rep.window.map(Anchor::Window)
}
