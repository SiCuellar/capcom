# AGENTS.md

Machine-friendly notes for AI coding agents that **install** Capcom for a user or **work on** this repo.

Repository: `https://github.com/SiCuellar/capcom`. Release assets have stable names, so `releases/latest/download/<asset>` always works.

| Asset | Contents |
|---|---|
| `Capcom-macos-universal.zip` | `Capcom.app` (arm64 + x86_64; ad-hoc signed, not notarized) |
| `capcom-macos-universal.tar.gz` | the bare `capcom` CLI binary |
| `capcom-linux-x86_64.tar.gz`, `capcom-linux-aarch64.tar.gz` | `capcom`, `packaging/linux/`, README, LICENSE |
| `SHA256SUMS.txt` | checksums |

## Install for a user

### 1. Ask the user first

Collect these before running anything:

- **Speech-to-text:** a URL ending in `/v1/audio/transcriptions` and a model id. Or a preset: `vllm`, `speaches`, `whisper-cpp`, `parakeet`, `groq`, `openai`.
- **Cleanup LLM:** a URL ending in `/v1/chat/completions` and a model id, a preset (`ollama`, `vllm`, `lmstudio`, `llama-cpp`, `groq`, `openai`), or "none".
- **API key:** whether the endpoints need one. One key is sent as `Authorization: Bearer` to both. Leave it out for local servers that don't check keys.

Run `capcom setup --list-presets` to see each preset's default URL and model. A flag always overrides the preset's value: for example, `--asr-preset vllm --asr-url http://gpu-box:8000/v1/audio/transcriptions`.

### 2. macOS

```sh
cd "$(mktemp -d)"
curl -fsSLO https://github.com/SiCuellar/capcom/releases/latest/download/Capcom-macos-universal.zip
ditto -x -k Capcom-macos-universal.zip .
rm -rf /Applications/Capcom.app && mv Capcom.app /Applications/
xattr -dr com.apple.quarantine /Applications/Capcom.app 2>/dev/null || true
mkdir -p ~/.local/bin && ln -sf /Applications/Capcom.app/Contents/MacOS/capcom ~/.local/bin/capcom
```

If `~/.local/bin` is not on `PATH`, call the binary by its full path.

### 3. Linux

```sh
arch=$(uname -m)   # x86_64 or aarch64
cd "$(mktemp -d)"
curl -fsSL https://github.com/SiCuellar/capcom/releases/latest/download/capcom-linux-$arch.tar.gz | tar xz
cd capcom-linux-$arch && ./packaging/linux/install.sh --no-build
```

The installer may run `sudo pacman` for missing deps (wl-clipboard, wtype, pipewire, libnotify, jq). It does not prompt for setup when stdin is not a TTY.

### 4. Configure (non-interactive)

```sh
# Key on stdin keeps it out of shell history and process lists:
printf '%s\n' "$KEY" | capcom setup --yes --asr-preset vllm --asr-url URL --asr-model MODEL \
    --llm-url URL --llm-model MODEL --api-key -
# No key, no cleanup:
capcom setup --yes --asr-url URL --asr-model MODEL --no-cleanup
```

- `--yes` (or a non-TTY stdin) never prompts. Missing values are taken from the existing config. If required values are still missing, setup exits non-zero with a message naming the missing flag.
- Setup writes `~/.config/capcom/config.toml` (0600), `~/.config/capcom/api_key` (0600) and `cleanup-prompt.txt` if it is absent. Other config sections are preserved.
- `--api-key ""` removes the stored key. `--skip-test` skips the live check.
- **Exit codes:** `0` = config written and live check passed. `1` = config written but a check failed, or bad flags. The output names the failing check.

### 5. Verify

```sh
capcom doctor        # exit 0 = all required checks passed
capcom config show   # effective config, key redacted. Never cat the api_key file.
```

On macOS, `doctor` also reports Microphone, Accessibility and Input Monitoring **for the binary that runs it**. Grants belong to `/Applications/Capcom.app`, so a terminal run may show FAIL even when the app is fine. Agents cannot grant these permissions. Tell the user:

1. Open Capcom.app.
2. Grant Microphone, Accessibility and Input Monitoring, plus Local Network if the server is on the LAN (System Settings → Privacy & Security).
3. Quit Capcom from the menu bar and open it again.
4. Hold right ⌃ to talk.

The daemon reads its config only at startup, so restart it after setup:

- macOS: quit from the menu bar, then `open /Applications/Capcom.app`.
- Linux: `systemctl --user restart capcom`.

### Rules

- Never echo, log or commit the API key.
- Do not paste into the user's apps while testing. For a sandbox daemon use `CAPCOM_SOCKET=/tmp/capcom-test.sock CAPCOM_NO_HOTKEY=1 CAPCOM_HEADLESS=1 capcom daemon`.
- Cloud presets (`groq`, `openai`) send the user's audio off-machine. Confirm with the user before using them.

## Working on the repo

```sh
cargo build --release      # target/release/capcom
cargo test
cargo clippy --all-targets -- -D warnings
packaging/macos/make-app.sh                     # Capcom.app (CAPCOM_UNIVERSAL=1 for arm64+x86_64)
python3 docs/assets/gen_demo.py docs/assets/demo.svg   # regenerate the README animation
```

Linux builds need `pkg-config` and ALSA headers. CI (`.github/workflows/ci.yml`) runs build, test and clippy on macOS and Ubuntu. Pushing a `v*` tag runs `release.yml`.

| Path | Role |
|---|---|
| `src/main.rs` | CLI (clap). A Finder launch with no args runs `daemon`. |
| `src/setup.rs` | `capcom setup` wizard and backend presets |
| `src/config.rs` | TOML schema, defaults (empty server = "not set up"), key loading |
| `src/asr.rs`, `src/cleanup.rs` | OpenAI-compatible HTTP clients. Bearer is sent only if a key exists. |
| `src/pipeline.rs` | WAV → ASR → optional cleanup → `Outcome`, falling back to raw text |
| `src/daemon.rs`, `src/state.rs`, `src/ipc.rs` | daemon, pure state machine, JSON-lines Unix socket |
| `src/audio/` | cpal capture, level metering, WAV helpers |
| `src/platform/macos/` | hotkey, paste, sounds, permissions, menu bar + pill UI |
| `src/platform/linux.rs` | Wayland paste (`wl-copy`/`wtype`), notifications, Waybar state |
| `packaging/` | macOS app bundle, Linux installer + Omarchy backend, Homebrew cask template |

Conventions:

- **Privacy:** no telemetry and no new network destinations.
- **Keep it light:** small dependencies, no async runtime on the main thread on macOS (AppKit owns it).
- New backends are **presets** in `src/setup.rs`, with tests and a README table row.
- Pure logic gets unit tests next to the code.
- Credit borrowed code in `THIRD_PARTY.md`.
- Commit messages are imperative and scoped, for example `feat(setup): …` or `fix(macos): …`.
