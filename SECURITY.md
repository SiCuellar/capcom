# Security & privacy

## What Capcom does with your data

- **No telemetry.** No analytics, crash reporting, update pings or license checks. The only network traffic is what you configure: audio to your `asr_url`, text to your `llm_url`.
- **Audio stays in memory.** Recordings are never written to disk; they are sent once and dropped.
- **Secrets are local and private.** The API key lives in `~/.config/capcom/api_key` (mode `0600`, never in `config.toml`), or in `CAPCOM_API_KEY`. `capcom config show` redacts it. The IPC socket is `0600`.
- **History is local.** Final transcripts go to `history.jsonl` in your data dir (`~/Library/Application Support/capcom/` on macOS, `~/.local/share/capcom/` on Linux). Turn it off with `[history] enabled = false`.
- **The clipboard is borrowed, not kept.** Pasting snapshots your clipboard and restores it afterwards.
- **Cloud backends are opt-in.** If you pick a hosted preset (OpenAI, Groq), your audio and text go to that provider under their terms.

Use `https://` or a private network (LAN, WireGuard, Tailscale) between Capcom and your servers; plain `http://` over the open internet exposes your dictation.

## Reporting a vulnerability

Please **do not open a public issue**. Use GitHub's private vulnerability reporting (repo → Security → *Report a vulnerability*). Include steps to reproduce and the Capcom version (`capcom --version`). We aim to reply within a week.
