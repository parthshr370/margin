//! ⌘K command palette: every registered command (with its shortcut) and every tool in the
//! catalogue, with a fuzzy-ish substring match; ↑ ↓ select a hit and Enter runs it. In Margin it
//! also lists Margin's own commands, shows the keymap's shortcuts and keeps one height while the
//! hits change.

use egui::{Align2, CornerRadius, Rect, Sense, vec2};
use pdfcraft_engine::catalog::{Availability, TOOL_GROUPS};

use crate::margin::{keys, look};
use crate::theme::{self, Tokens};
use crate::{Flavor, PdfCraftApp, icons};

/// The most hits listed, and one row's height: Margin's list keeps room for all of them.
const MAX_HITS: usize = 12;
const ROW_HEIGHT: f32 = 36.0;

struct Hit {
    /// The tool panel to open (tools and catalogue items); `None` for plain commands.
    group: Option<&'static str>,
    label: String,
    detail: String,
    icon: String,
    command: Option<String>,
    ready: bool,
}

/// Where `needle` (folded with [`pdfcraft_render::text::fold_case`]) matches `hay`, for ranking.
fn score(hay: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let h = pdfcraft_render::text::fold_case(hay);
    if let Some(p) = h.find(needle) {
        return Some(p);
    }
    // A right-to-left language's labels are in display order (their words run the other way)
    // while the query is in typing order, so several words match one by one.
    if crate::i18n::current().rtl() && needle.contains(char::is_whitespace) {
        let first = needle.split_whitespace().map(|word| h.find(word)).try_fold(usize::MAX, |first, p| p.map(|p| first.min(p)));
        if let Some(first) = first {
            return Some(first);
        }
    }
    // Subsequence match as a fallback.
    let mut it = h.chars();
    needle.chars().all(|c| it.any(|x| x == c)).then_some(100)
}

pub fn show(app: &mut PdfCraftApp, ctx: &egui::Context) {
    let palette_rect_id = egui::Id::new("palette_rect");
    if !app.palette_open {
        return;
    }
    let t = Tokens::get(ctx);
    if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.palette_open = false;
        return;
    }
    let margin = app.flavor == Flavor::Margin;
    let q = pdfcraft_render::text::fold_case(app.palette_query.trim());
    let hits = hits(app, ctx, &q, margin);
    let selected = move_selection(ctx, &q, hits.len(), margin);

    let screen = ctx.content_rect();

    let previous_rect = ctx.data(|data| data.get_temp::<Rect>(palette_rect_id));

    let outside_click = ctx.input(|input| {
        input.pointer.button_pressed(egui::PointerButton::Primary)
            && input.pointer.interact_pos().is_some_and(|pos| previous_rect.is_some_and(|rect| !rect.contains(pos)))
    });

    if outside_click {
        app.palette_open = false;
        app.palette_query.clear();
        return;
    }
    let mut chosen: Option<(Option<String>, Option<&'static str>)> = None;
    let area_response = egui::Area::new(egui::Id::new("palette"))
        .order(egui::Order::Foreground)
        .pivot(Align2::CENTER_TOP)
        .fixed_pos(egui::pos2(screen.center().x, screen.top() + 96.0))
        .show(ctx, |ui| {
            let frame = if margin {
                look::floating_frame(&t, look::RADIUS_FLOATING).inner_margin(egui::Margin::same(10))
            } else {
                egui::Frame::popup(ui.style()).corner_radius(CornerRadius::same(12)).inner_margin(egui::Margin::same(10))
            };
            frame.show(ui, |ui| {
                ui.set_width(560.0);
                ui.horizontal(|ui| {
                    ui.add(icons::image("search", 18.0, t.text_muted));
                    let r = ui.add(
                        egui::TextEdit::singleline(&mut app.palette_query)
                            .hint_text(tl!("Search tools and commands…"))
                            .frame(egui::Frame::NONE)
                            .font(theme::regular(15.0))
                            .desired_width(f32::INFINITY),
                    );
                    // Enter runs the selected hit (↑ ↓ move it). The field keeps focus (requested
                    // every frame), so check while it is focused as well as when focus is lost.
                    if (r.has_focus() || r.lost_focus())
                        && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        && let Some((_, h)) = hits.get(selected)
                    {
                        chosen = Some((h.command.clone(), h.group));
                    }
                    r.request_focus();
                });
                ui.separator();
                ui.vertical(|ui| {
                    // Margin's list keeps the room of a full list, so the panel never changes
                    // size while typing.
                    if margin {
                        ui.set_min_height(MAX_HITS as f32 * ROW_HEIGHT);
                    }
                    for (row, (_, h)) in hits.iter().enumerate() {
                        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
                        let body = if margin {
                            look::row_fill(ui, &t, rect, &resp, row == selected);
                            rect
                        } else {
                            let press = theme::Press::track(ui, &resp);
                            press.wash(ui, rect, 6, row == selected);
                            rect.translate(press.offset())
                        };
                        icons::paint(ui, Rect::from_min_size(body.min + vec2(8.0, 9.0), vec2(18.0, 18.0)), &h.icon, 17.0, t.icon);
                        // Display is translated; matching above already considered both languages.
                        let shown = crate::i18n::command_label(&h.label);
                        let shown_detail = tl!(&h.detail).to_string();
                        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, h.ready, &shown));
                        let fg = if h.ready { t.text } else { t.text_faint };
                        let detail_font = theme::regular(12.0);
                        let detail_max = (rect.width() * 0.42).clamp(0.0, 220.0);
                        let detail = crate::widgets::fit_line(&shown_detail, detail_max, |s| crate::widgets::text_width(ui, s, &detail_font));
                        let detail_w = crate::widgets::text_width(ui, &detail, &detail_font);
                        ui.painter().text(body.right_center() - vec2(10.0, 0.0), Align2::RIGHT_CENTER, detail, detail_font, t.text_faint);
                        crate::widgets::paint_left(
                            ui,
                            body.left_center() + vec2(36.0, 0.0),
                            &shown,
                            theme::regular(13.5),
                            fg,
                            (rect.width() - 36.0 - detail_w - 24.0).max(0.0),
                        );
                        if theme::hand(resp).clicked() {
                            chosen = Some((h.command.clone(), h.group));
                        }
                    }
                    if hits.is_empty() {
                        ui.label(egui::RichText::new(tl!("No matching tools")).color(t.text_muted));
                    }
                });
                ui.add_space(2.0);
            });
        });

    ctx.data_mut(|data| {
        data.insert_temp(palette_rect_id, area_response.response.rect);
    });

    if let Some((command, group)) = chosen {
        app.palette_open = false;
        app.palette_query.clear();
        if let Some(g) = group {
            app.select_tool(g);
        }
        if let Some(c) = command {
            if !margin {
                app.run_command(&c);
            } else if !keys::run(app, ctx, &c, &serde_json::Value::Null) {
                log::warn!("palette: nothing runs {c}");
            }
        }
    }
}

/// The selected row among `count` hits for query `q`: ↑ and ↓ move it, a new query starts it at
/// the top. Margin consumes the keys, so the field doesn't take them.
fn move_selection(ctx: &egui::Context, q: &str, count: usize, margin: bool) -> usize {
    let (down, up) = if margin {
        ctx.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)))
    } else {
        ctx.input(|i| (i.key_pressed(egui::Key::ArrowDown), i.key_pressed(egui::Key::ArrowUp)))
    };
    let last = count.saturating_sub(1);
    ctx.data_mut(|data| {
        let id = egui::Id::new("palette_selected");
        let query_id = egui::Id::new("palette_selected_query");
        let previous_query = data.get_temp::<String>(query_id);
        let selected = if previous_query.as_deref() == Some(q) { data.get_temp::<usize>(id).unwrap_or(0) } else { 0 }.min(last);
        data.insert_temp(query_id, q.to_owned());
        let selected = if down {
            selected.saturating_add(1).min(last)
        } else if up {
            selected.saturating_sub(1)
        } else {
            selected
        };
        data.insert_temp(id, selected);
        selected
    })
}

/// The hits for query `q` (folded with [`pdfcraft_render::text::fold_case`]), best first, at most
/// [`MAX_HITS`]: PdfCraft's commands, its tools and their items, and in Margin (`margin`) Margin's
/// own commands that take no arguments, with the keymap's shortcuts.
fn hits(app: &PdfCraftApp, ctx: &egui::Context, q: &str, margin: bool) -> Vec<(usize, Hit)> {
    let mut hits: Vec<(usize, Hit)> = Vec::new();
    let active = app.active_ids().map(|(_, id)| id);
    for spec in pdfcraft_engine::commands::COMMANDS {
        let label = pdfcraft_engine::commands::current_label(spec, &app.session, active);
        let translated = crate::i18n::command_label(&label);
        if let Some(s) = score(&translated, q).or_else(|| score(&label, q)).or_else(|| score(spec.id, q).map(|s| s + 50)) {
            let menu = || tl!(spec.menu.unwrap_or("Command")).to_string();
            let detail = if margin {
                look::shortcut(app, spec.id).unwrap_or_else(menu)
            } else {
                spec.shortcut.map(|k| crate::commands::shortcut_label(ctx, k)).unwrap_or_else(menu)
            };
            hits.push((
                s,
                Hit {
                    group: None,
                    label: translated,
                    detail,
                    icon: spec.icon.to_owned(),
                    command: Some(spec.id.to_owned()),
                    ready: app.command_enabled(spec),
                },
            ));
        }
    }
    if margin && let Some(registry) = app.margin_keys.registry() {
        // Commands that need arguments are reached through their own controls and keys.
        for entry in registry.margin_entries().filter(|entry| entry.args.is_none()) {
            let id = entry.id.as_str();
            let found = score(&entry.title, q)
                .or_else(|| entry.aliases.iter().find_map(|alias| score(alias, q)).map(|s| s + 10))
                .or_else(|| score(id, q).map(|s| s + 50));
            if let Some(s) = found {
                hits.push((
                    s,
                    Hit {
                        group: None,
                        label: entry.title.clone(),
                        detail: look::shortcut(app, id).unwrap_or_else(|| tl!("Command").to_string()),
                        icon: entry.icon.clone().unwrap_or_else(|| "square".to_owned()),
                        command: Some(id.to_owned()),
                        ready: !keys::needs_document(id) || app.active.is_some(),
                    },
                ));
            }
        }
    }
    for g in TOOL_GROUPS {
        if let Some(s) = score(tl!(g.label), q).or_else(|| score(g.label, q)) {
            hits.push((
                s,
                Hit {
                    group: Some(g.id),
                    label: tl!(g.label).to_string(),
                    detail: tl!("Tool").into(),
                    icon: g.icon.to_owned(),
                    command: None,
                    ready: g.availability == Availability::Ready,
                },
            ));
        }
        for sec in g.sections {
            for i in sec.items {
                if pdfcraft_engine::commands::command(i.command).is_some() {
                    continue; // listed above as a command
                }
                if let Some(s) = score(tl!(i.label), q).or_else(|| score(i.label, q)).or_else(|| score(i.command, q).map(|s| s + 50)) {
                    hits.push((
                        s + 1,
                        Hit {
                            group: Some(g.id),
                            label: tl!(i.label).to_string(),
                            detail: tl!(g.label).into(),
                            icon: i.icon.to_owned(),
                            command: Some(i.command.to_owned()),
                            ready: i.availability == Availability::Ready,
                        },
                    ));
                }
            }
        }
    }
    hits.sort_by_key(|(s, h)| (*s, !h.ready));
    hits.truncate(MAX_HITS);
    hits
}

#[cfg(test)]
mod search {
    use super::score;
    use pdfcraft_render::text::fold_case;

    #[test]
    fn commands_match_in_any_case_with_turkish_i() {
        assert_eq!(score("İçe aktar", &fold_case("içe")), Some(0));
        assert_eq!(score("Yazdır", &fold_case("YAZDIR")), Some(0));
        assert_eq!(score("Insert Pages", &fold_case("ınsert")), Some(0));
        assert_eq!(score("Print", &fold_case("print")), Some(0));
        assert_eq!(score("Print", &fold_case("zz")), None);
    }
}
