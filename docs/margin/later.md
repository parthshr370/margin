# Margin: later work

Features we want but have not started. File references are from commit `d04c54d`.

## Read aloud

No text-to-speech code exists. The reading-order text is good: word-F1 0.98 against pdftotext
(`crates/render/src/text.rs` `layout`). Use the macOS speech synthesizer (speech-dispatcher on
Linux) and highlight each word as it is spoken, using the `PageText` glyph boxes. Tagged-PDF alt
text (`crates/a11y/src/alt.rs`) can voice figures. Effort: medium.

## Selection bar actions

Add Define (macOS Dictionary), Search web, Find in document and Speak to the selection bar
(`crates/ui-egui/src/margin/selection.rs`). Find is not filled in from the selection today.

## Compare versions and carry highlights over

The diff engine works (`crates/compare/src/lib.rs` `compare`), but the dialog needs the old
version to be open already (`crates/ui-egui/src/compare_ui.rs`).

1. Let the user pick the old version from disk. Small.
2. Expose the word alignment, which is private today (`crates/compare/src/lib.rs` `Op`), and move
   highlights from one version (for example arXiv v1) to the next (v2). Medium.

## Reflow mode

Reflow builds paragraphs in content order (`crates/edit/src/text.rs` `text_lines`), not the
column-aware reading order (`crates/render/src/text.rs` `layout`), so two-column papers would
probably come out mixed. Fix that in the engine first, then build a flowing view in Margin's
typography and page colours. Effort: medium to large.

## AI

The MCP server and the automation tools already exist (`crates/automation`), and the control
channel is connected in `apps/margin/src/main.rs`.

1. Document how to use Claude Desktop or Claude Code on the library and highlights through MCP.
   Small.
2. A local provider (for example Ollama) inside the app for "summarise" and "ask the paper", using
   `text_extract` plus the highlights. The catalogue already has `ai.summary`, `ai.ask` and
   `ai.translate` as `Provider` items (`crates/engine/src/catalog.rs`). Large.

## Reading stats

- A page read for more than 5 minutes with no input counts as 0
  (`crates/margin-config/src/stats.rs` visit handling). Cap each page at a maximum time instead,
  like KOReader.
- In two-up and scroll layouts only the current page gets time.
- Re-reading distorts the pace (total seconds divided by distinct pages).
- Add a KOReader `statistics.sqlite` import. It is cheap because Margin already uses KOReader's
  partial-MD5 fingerprint.
- Reading goals, per-book history and books per year.

## Sync and backup

Needs stable book identity first. The sidecar's `revision` field and `deleted[]` tombstones are
already designed for merging (`crates/margin-config/src/sidecar.rs`). Add export and import of
the library and stats, and never sync the live WAL databases through a cloud folder.
