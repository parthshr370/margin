---
name: margin-config
description: Margin config. Use to change Margin's settings, key bindings or theme, or to write a Margin theme (settings.jsonc, keymap.jsonc, themes.json).
---

Margin is configured the way Zed is: plain JSONC files the user owns. Margin reloads every file the moment it is saved. The Settings window covers a few common choices (theme mode and theme) and writes them into the same files, so the files are always the whole truth.

## Steps

1. **Find `<config>`.** It is `$MARGIN_CONFIG_DIR` if that is set. Otherwise it is `~/Library/Application Support/margin` on macOS, or `~/.config/margin` on Linux.
   - Done when you have listed the folder and seen `settings.jsonc`.
   - If the folder is missing, Margin has never run: ask the user to start it once, which writes every file below.
2. **Look up every key or command you will write**, in the files Margin generates. Never write from memory: these files are the source of truth and are regenerated on each launch.
   - Settings: `default-settings.jsonc` lists every key with its default and meaning, and `schemas/settings.schema.json` gives the allowed values.
   - Key bindings: the header of `keymap.jsonc` lists every command id, the contexts and the key syntax.
   - Themes: `schemas/theme.schema.json` lists every style key. The built-in themes are "Margin Dark" and "Margin Light".
   
   Done when each key, value and command id you plan to write appears in one of these files.
3. **Edit the user's file in place:**
   - Keep the user's comments and order; the files are JSONC, so comments and trailing commas are fine.
   - Write only the keys that differ from the defaults, nested by their dotted path: `view.zoom` becomes `"view": { "zoom": 120 }`.
   - Leave `default-settings.jsonc` and `schemas/` alone; Margin overwrites them.
   - Done when every change the user asked for is in the file and nothing else changed.
4. **Verify.** If Margin is running, it reloads within a second. A bad value shows a notice in the app and adds a warning to `<config>/logs/pdfcraft.log` naming the file, line and key; Margin uses the default instead.
   - Done when the log's lines after your save hold no warning for your file. Fix and re-save until that is true.
   - If Margin is not running, the check happens at its next start.

## Themes

- **Where themes go:** write them in `<config>/themes.json`. It is a theme family:
  `{ "$schema": "./schemas/theme.schema.json", "name": "…", "author": "…", "themes": [ { "name": "…", "appearance": "dark" | "light", "extends": "Margin Dark", "style": { "<key>": "#rrggbb" | "#rrggbbaa" } } ] }`.
  `extends` fills every key a theme leaves out, so set only the keys that change.
- **Zed themes:** a Zed theme file works as-is when dropped into `<config>/themes/`. Margin ignores the Zed keys it has no use for.
- **Choosing a theme:** set `"theme"` in `settings.jsonc`, either `"My Theme"` or `{ "mode": "system" | "light" | "dark", "light": "…", "dark": "…" }`.
- **Changing one colour of an existing theme:** use `"theme_overrides": { "<theme name>": { "<key>": "#…" } }` in `settings.jsonc` instead of writing a whole theme.
