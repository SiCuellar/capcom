//! macOS additions to `capcom doctor`.

use super::secure;
use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

pub const URL_ACCESSIBILITY: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility";
pub const URL_INPUT_MONITORING: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent";
pub const URL_MICROPHONE: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone";

fn mic_status() -> &'static str {
    let status = unsafe {
        match AVMediaTypeAudio {
            Some(t) => AVCaptureDevice::authorizationStatusForMediaType(t),
            None => return "unknown",
        }
    };
    if status == AVAuthorizationStatus::Authorized {
        "authorized"
    } else if status == AVAuthorizationStatus::NotDetermined {
        "not determined (will prompt on first recording)"
    } else if status == AVAuthorizationStatus::Denied {
        "denied"
    } else {
        "restricted"
    }
}

/// Ask for microphone access up front if macOS has never been asked, so the system prompt
/// appears at launch instead of the first dictation silently recording zeros (CoreAudio gives an
/// unauthorized app silence, not an error). Non-blocking; the answer is just logged.
pub fn request_mic_access() {
    let Some(media) = (unsafe { AVMediaTypeAudio }) else { return };
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
    if status == AVAuthorizationStatus::NotDetermined {
        let handler = block2::RcBlock::new(|granted: objc2::runtime::Bool| {
            tracing::info!("microphone access {}", if granted.as_bool() { "granted" } else { "denied" });
        });
        unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &handler) };
    } else if status != AVAuthorizationStatus::Authorized {
        tracing::warn!("microphone access is {}: dictation will record silence; enable Capcom in {URL_MICROPHONE}", mic_status());
    }
}

/// Print the macOS checks. Returns true if nothing required is missing.
/// Note: permissions are per executable identity; run this from the Capcom.app bundle binary
/// (`/Applications/Capcom.app/Contents/MacOS/capcom doctor`) to test what the daemon will have.
pub fn run() -> bool {
    let ax = secure::accessibility_trusted();
    let im = secure::input_monitoring_granted();
    let mic = mic_status();
    let secure_on = secure::secure_input_enabled();
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    println!("[info] macOS permissions are checked for this executable: {exe}");
    let tag = |ok: bool| if ok { " ok " } else { "FAIL" };
    println!("[{}] Accessibility (paste + hotkey): {}", tag(ax), if ax { "trusted" } else { "NOT trusted" });
    println!("       {URL_ACCESSIBILITY}");
    println!("[{}] Input Monitoring (hotkey): {}", tag(im), if im { "granted" } else { "NOT granted" });
    println!("       {URL_INPUT_MONITORING}");
    let mic_ok = mic.starts_with("authorized") || mic.starts_with("not determined");
    println!("[{}] Microphone: {mic}", tag(mic_ok));
    println!("       {URL_MICROPHONE}");
    if secure_on {
        println!("[warn] Secure Input is ON right now (password field or Terminal 'Secure Keyboard Entry'): pasting and hotkeys may not work");
    } else {
        println!("[ ok ] Secure Input: off");
    }
    println!("[info] set System Settings > Keyboard > 'Press Globe key to' = Do Nothing, or Fn will open the emoji picker / dictation");
    ax && im && mic_ok
}
