-- Plain Hyprland (Lua config) push-to-talk for Capcom.
-- NOT needed on Omarchy: Omarchy already binds F9 / Right Alt (hold) and Super+Ctrl+X (toggle)
-- to `omarchy-dictation`, which calls the omarchy-dictation-capcom backend.
--
-- Usage: copy to ~/.config/hypr/ and `require("hyprland-binds")` from hyprland.lua.
-- Hold-to-talk: press runs `capcom start`, release runs `capcom stop`. The release bind ignores
-- modifiers and is non-consuming, same as Omarchy's o.bind_hold (default/hypr/helpers.lua),
-- so it still fires if you type or change modifiers while holding.

local held = false

hl.bind("F9", function()
  held = true
  hl.exec_cmd("capcom start")
end, { description = "Capcom: start dictation (hold)" })

hl.bind("F9", function()
  if held then
    held = false
    hl.exec_cmd("capcom stop")
  end
end, { description = "Capcom: stop dictation (release)", release = true, transparent = true, ignore_mods = true, non_consuming = true })

hl.bind("SUPER + CTRL + X", hl.dsp.exec_cmd("capcom toggle"), { description = "Capcom: toggle dictation" })
hl.bind("SUPER + CTRL + ESCAPE", hl.dsp.exec_cmd("capcom cancel"), { description = "Capcom: cancel dictation" })
