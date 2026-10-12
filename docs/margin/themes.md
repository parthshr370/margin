# Themes

Margin is themed the way Zed is: you pick a theme in `settings.jsonc` and write your own themes as
JSON. Margin applies every save at once, without a restart.

## Picking a theme

In `<config>/settings.jsonc` (`~/.config/margin/` on Linux, `~/Library/Application Support/margin/`
on macOS):

```jsonc
// One theme, always:
"theme": "Margin Light",

// Or one theme per appearance; "system" follows your computer's light or dark setting:
"theme": { "mode": "system", "light": "Margin Light", "dark": "Margin Dark" }
```

The default is `{ "mode": "dark", "light": "Margin Light", "dark": "Margin Dark" }`. The built-in
themes are **Margin Dark** (Margin's black reader palette) and **Margin Light**. Settings ▸
Appearance sets the same keys, and its buttons open `settings.jsonc` and `themes.json`. The command
palette has **Open settings file**, **Open default settings**, **Open themes file** and **Open
keymap file**.

To change a few colours of a theme without writing a new one, use `theme_overrides`:

```jsonc
"theme_overrides": {
  "Margin Dark": { "text.accent": "#4b8bf5", "editor.background": "#0b0b0b" }
}
```

## Where themes go

| File | What it is |
|---|---|
| `<config>/themes.json` | Your themes. Margin writes it on first start with one example theme. |
| `<config>/themes/*.json` | Drop-in theme files, read in file-name order. Zed theme files work here as they are. |
| `<config>/schemas/theme.schema.json` | The schema: every style key, for editor completion. |

A theme name defined twice is reported, and the later definition wins (built-ins first, then
`themes.json`, then `themes/*.json`).

## The format

A theme file is a theme family, as in Zed:

```jsonc
{
  "$schema": "./schemas/theme.schema.json",
  "name": "My themes",
  "author": "You",
  "themes": [
    {
      "name": "Dusk",
      "appearance": "dark",          // "dark" or "light"
      "extends": "Margin Dark",      // optional
      "style": {
        "text.accent": "#4b8bf5",
        "background": "#101114",
        "editor.background": "#0b0c0e",
        "panel.background": "#14161a",
        "margin.radius": 4
      }
    }
  ]
}
```

- `extends` names the theme whose values fill every key this one leaves out: a built-in, or any
  other theme. Without it, a dark theme starts from Margin Dark and a light one from Margin Light.
  A loop or a parent that doesn't exist is reported, and the theme then starts from the built-in
  of its appearance.
- Colours are `#rrggbb` or `#rrggbbaa`. `null` leaves a key unset.
- A bad colour, or a key Margin doesn't know, is reported (a notice, and a warning in
  `<config>/logs/` naming the file, line and key) and the parent theme's value is used. The rest of
  the theme still applies.

## Style keys

Keys with Zed's names colour what they colour in Zed. Margin's own roles start with `margin.`.
Keys listed later refine keys listed earlier.

| Key | What it colours |
|---|---|
| `background` | The header, the tab strip and the footer. |
| `editor.background` | Behind the pages, and the library's background. |
| `panel.background` | The sidebar and its lists. |
| `surface.background` | Cards: library books, highlight cards, stats cards. |
| `elevated_surface.background` | The annotate rail, popovers, menus, tooltips, toasts and sheets. |
| `title_bar.background` | The header and the tab strip. |
| `status_bar.background` | The footer under a document. |
| `border` | The edge between regions and around cards and floating surfaces (`#00000000`: none). |
| `border.variant` | Rules between rows and sections, and control outlines. |
| `border.selected` | The outline of a selected row, chip, segment, card or tab. |
| `element.background` | Input wells, dropdowns and segmented controls. |
| `element.hover`, `ghost_element.hover` | A hovered row or control. |
| `element.active`, `ghost_element.active` | A pressed row or control. |
| `element.selected`, `ghost_element.selected` | A selected row, chip, segment or tab. |
| `text` | Body text and labels. |
| `text.muted` | Descriptions, secondary columns, inactive labels. |
| `text.placeholder` | Hints, placeholders, counts, the faintest captions. |
| `icon`, `icon.muted` | Icons at rest. |
| `margin.track` | Empty progress tracks, slider rails, empty days in the activity calendar. |
| `text.accent` | The accent: every accent role at once (fills, headings, selected labels, active icons, the focus ring, text selection, find matches, the activity ramp). |
| `margin.accent` | The accent fill alone (primary buttons, switches, progress, focus ring) and its shades. |
| `border.focused` | The keyboard focus ring, a focused field's outline, the text cursor. |
| `element.selection_background` | Selected text. |
| `search.match_background` | Matched words in the Find pane's results. |
| `margin.accent.hover`, `margin.accent.active` | The accent fill under the pointer and while pressed. |
| `margin.on_accent` | Text and icons on an accent fill. |
| `margin.heading` | Section titles (Stats, Settings, tool groups). |
| `margin.section_label` | Small-caps section labels (CONTENTS, RECENT). |
| `margin.selected_text` | The label of a selected row, chip, segment or card. |
| `margin.control_border` | Outlines of inputs, secondary buttons and segmented controls. |
| `margin.border.hover` | The edge of a hovered row, card or control. |
| `margin.scrim` | The shade behind a modal sheet. |
| `margin.heat.1` … `margin.heat.4` | The activity ramp, lightest to busiest. |
| `margin.page_shadow` | The shadow under each page. |
| `margin.radius` | A control's corner radius in points, 0 to 16 (a number). |

### Zed keys Margin doesn't use

Every other key of Zed's theme schema is accepted and changes nothing, so Zed themes load without
warnings: `background.appearance`, `accents`, `players`, `syntax`, the `terminal.*`, `editor.*`
(except `editor.background`), `scrollbar.*`, `minimap.*`, `version_control.*` and `vim.*` keys,
the status colours (`error`, `warning`, `info`, `success`, `hint`, `created`, `deleted`,
`modified`, `renamed`, `conflict`, `hidden`, `ignored`, `predictive`, `unreachable` and their
`.background` and `.border`), `border.transparent`, `border.disabled`, `element.disabled`,
`ghost_element.background`, `ghost_element.disabled`, `text.disabled`, `icon.disabled`,
`icon.placeholder`, `icon.accent`, `debugger.accent`, `drop_target.*`,
`title_bar.inactive_background`, `toolbar.background`, `tab_bar.background`, `tab.*`,
`search.active_match_background`, `panel.focused_border`, `panel.indent_guide*`,
`panel.overlay_*`, `pane.focused_border`, `pane_group.border` and `link_text.hover`.
`schemas/theme.schema.json` lists them all.

Page colours (Paper, Sepia, Night, Ink, Mono) and the highlight palette are not part of a theme:
the page modes are set in the Display popover and `view.page_color`, and highlight colours in
`notes.colors`.
