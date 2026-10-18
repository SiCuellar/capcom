# Third-party code and ideas

## Handy (MIT) - https://github.com/cjpais/Handy
Copyright (c) CJ Pais. The macOS paste recipe (pasteboard snapshot, transient/auto-generated
marker types, Cmd+V via CGEvent, changeCount-guarded restore, Secure Input detection) in
`src/platform/macos/` follows Handy's `paste_tx/macos.rs`, `clipboard.rs` and
`secure_input.rs`. Implemented independently; credited for the approach.

## handy-keys (MIT) - https://github.com/handy-computer/handy-keys
Used as a dependency for the macOS global hotkey (CGEventTap).

## Voxtype (MIT) - https://github.com/peteonrails/voxtype
The Wayland output approach in `src/platform/linux.rs` is borrowed as ideas: clipboard save/restore via `wl-paste`/`wl-copy`, and a `wtype` fallback (see Voxtype's `src/output/paste.rs`).

## Omarchy (MIT) - https://github.com/omacom/omarchy
- **Paste chord** in `src/platform/linux.rs`: the `hl.dsp.send_key_state` down/up split, the `sendshortcut` fallback and the terminal class list. Sources: `default/hypr/bindings/clipboard.lua`, `bin/omarchy-menu-keybindings`, `default/hypr/apps/terminals.lua`.
- **Dictation backend scripts** in `packaging/linux/omarchy/`: modelled on `bin/omarchy-{dictation,install-dictation,remove-dictation}-voxtype`.

## VoiceInk (GPLv3)
Ideas only; no code was copied.
