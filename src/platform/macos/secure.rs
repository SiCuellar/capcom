//! Permission / secure-input probes (all non-prompting).

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn IsSecureEventInputEnabled() -> u8;
}

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightListenEventAccess() -> bool;
}

/// True when some process has enabled Secure Event Input (password field, secure keyboard entry).
pub fn secure_input_enabled() -> bool {
    unsafe { IsSecureEventInputEnabled() != 0 }
}

/// Accessibility ("trusted") for this process; required to post Cmd+V and to run the event tap.
pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Input Monitoring for this process, without triggering the system prompt.
pub fn input_monitoring_granted() -> bool {
    unsafe { CGPreflightListenEventAccess() }
}
