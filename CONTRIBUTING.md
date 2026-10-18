# Contributing

Thanks for helping Capcom! Bug reports, backend recipes and small focused PRs are all welcome.

## Dev loop

```sh
cargo build --release   # target/release/capcom
cargo test
cargo clippy --all-targets -- -D warnings
```

Linux needs `pkg-config` and ALSA headers (`libasound2-dev` / `alsa-lib`). macOS app bundle: `packaging/macos/make-app.sh` (ad-hoc signed unless you have a signing identity; see `create-signing-cert.sh` to keep permission grants across rebuilds).

Test against a throwaway daemon so you never paste into your real apps:

```sh
CAPCOM_SOCKET=/tmp/capcom-dev.sock CAPCOM_NO_HOTKEY=1 CAPCOM_HEADLESS=1 target/release/capcom daemon
```

## Ground rules

- **Privacy first.** No telemetry, no new network destinations beyond the user's configured endpoints, no secrets in logs.
- **Stay light.** Capcom idles at ~20 MB; think twice before adding heavy dependencies.
- **OpenAI-compatible only.** New backends should be presets (`src/setup.rs`), not new protocols.
- Tests for pure logic (state machine, parsing, presets) live next to the code. CI runs build, test and clippy on macOS and Linux.
- Credit borrowed ideas or code in `THIRD_PARTY.md`.
- Keep PRs small and describe how you tested them (OS, backend, models).

By contributing you agree your work is released under the MIT license.
