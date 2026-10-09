//! Margin, the reader-first flavour of this UI ([`crate::Flavor::Margin`]). Everything here is
//! Margin-only; upstream files reach it through small hooks listed in Margin's `RESKIN-HOOKS.md`.
//!
//! The reader layout: a header with the document's title and section, a
//! left sidebar with tabs, a floating annotate rail over the page and a footer with the page and
//! zoom. Every control runs an existing PdfCraft command or panel; this module only arranges them.
//!
//! Margin's interface text is English for now: these strings are plain literals, not `tl!`, so
//! PdfCraft's catalogs (which must cover every `tl!` literal) stay complete.

pub mod display;
pub mod footer;
pub mod header;
pub mod keys;
pub mod keysheet;
pub mod obsidian;
pub mod pagecolor;
pub mod rail;
pub mod settings_watch;
pub mod sidebar;

use crate::{PdfCraftApp, RightPanel};

/// What the reader chrome shows. Not saved between runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderChrome {
    pub sidebar_open: bool,
    pub tab: SidebarTab,
    /// The annotate rail floats over the page.
    pub rail_open: bool,
    /// The right panel the chrome last asked for. When `app.right` differs, a command or option
    /// picked a panel since, and the sidebar follows it.
    synced_right: Option<RightPanel>,
    /// The document shown when the chrome last synced; another one starts on Contents.
    synced_doc: Option<pdfcraft_engine::DocId>,
}

impl Default for ReaderChrome {
    fn default() -> Self {
        Self { sidebar_open: true, tab: SidebarTab::Contents, rail_open: true, synced_right: None, synced_doc: None }
    }
}

/// The sidebar's tabs, in the order of its tab strip; `Other` shows a PdfCraft panel that has no
/// tab of its own (Signatures, Layers…), opened from the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarTab {
    Contents,
    Highlights,
    Pages,
    Bookmarks,
    Find,
    Tools,
    Other(RightPanel),
}

impl SidebarTab {
    pub const STRIP: [Self; 6] = [Self::Contents, Self::Highlights, Self::Pages, Self::Bookmarks, Self::Find, Self::Tools];

    pub fn label(self) -> &'static str {
        match self {
            Self::Contents => "Contents",
            Self::Highlights => "Highlights",
            Self::Pages => "Pages",
            Self::Bookmarks => "Bookmarks",
            Self::Find => "Find",
            Self::Tools => "Tools",
            Self::Other(_) => "Panel",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Contents => "list",
            Self::Highlights => "highlighter",
            Self::Pages => "layout-grid",
            Self::Bookmarks => "bookmark",
            Self::Find => "search",
            Self::Tools => "wrench",
            Self::Other(_) => "panel-left",
        }
    }

    /// The PdfCraft panel whose state this tab drives (thumbnails render only while Pages is
    /// shown, the find bar hands its search to the Find tab).
    pub fn panel(self) -> Option<RightPanel> {
        match self {
            Self::Highlights => Some(RightPanel::Comments),
            Self::Pages => Some(RightPanel::Pages),
            Self::Bookmarks => Some(RightPanel::Bookmarks),
            Self::Find => Some(RightPanel::Search),
            Self::Other(p) => Some(p),
            Self::Contents | Self::Tools => None,
        }
    }

    pub fn for_panel(panel: RightPanel) -> Self {
        match panel {
            RightPanel::Comments => Self::Highlights,
            RightPanel::Pages => Self::Pages,
            RightPanel::Bookmarks => Self::Bookmarks,
            RightPanel::Search => Self::Find,
            other => Self::Other(other),
        }
    }

    /// The `tab` argument of `view.sidebar.tab` (see [`run`]).
    pub fn key(self) -> Option<&'static str> {
        match self {
            Self::Contents => Some("contents"),
            Self::Highlights => Some("highlights"),
            Self::Pages => Some("pages"),
            Self::Bookmarks => Some("bookmarks"),
            Self::Find => Some("find"),
            Self::Tools => Some("tools"),
            Self::Other(_) => None,
        }
    }
}

/// Margin's defaults on a new app (saved settings may override them afterwards).
pub(crate) fn apply_defaults(app: &mut PdfCraftApp) {
    // Picking a comment tool must not switch the sidebar to Highlights (see `sync`).
    app.comments_panel_closed = true;
}

/// Run a Margin-only action (the keymap's ids): `view.sidebar.toggle`, `view.sidebar.tab` with
/// `{"tab": "contents" | "highlights" | "pages" | "bookmarks" | "find" | "tools"}`, and
/// `reader.annotate.toggle` (the rail). Returns `false` for an unknown id or tab.
pub fn run(app: &mut PdfCraftApp, id: &str, args: &serde_json::Value) -> bool {
    match id {
        "view.sidebar.toggle" => app.margin_chrome.sidebar_open = !app.margin_chrome.sidebar_open,
        "reader.annotate.toggle" => app.margin_chrome.rail_open = !app.margin_chrome.rail_open,
        "view.sidebar.tab" => {
            let wanted = args.get("tab").and_then(serde_json::Value::as_str);
            match SidebarTab::STRIP.into_iter().find(|tab| tab.key().is_some() && tab.key() == wanted) {
                Some(tab) => open_tab(app, tab),
                None => return false,
            }
        }
        _ => return false,
    }
    true
}

/// Whether annotating is on: the rail shows.
pub fn annotate_mode(app: &PdfCraftApp) -> bool {
    app.margin_chrome.rail_open
}

/// Show `tab` in the sidebar, opening the sidebar if needed.
pub(crate) fn open_tab(app: &mut PdfCraftApp, tab: SidebarTab) {
    let chrome = &mut app.margin_chrome;
    chrome.sidebar_open = true;
    chrome.tab = tab;
    if let (SidebarTab::Find, Some(view)) = (tab, app.active.and_then(|i| app.views.get_mut(i))) {
        view.open_find();
    }
}

/// Keep PdfCraft's panel state and the sidebar in step, once per frame before drawing.
///
/// A newly shown document starts on Contents, or on the panel its initial view asks for.
/// Commands (`comment.list`, `edit.advanced_search`, `sign.panel`, the control channel's
/// `panel` option) still set `app.right`: the sidebar switches to the matching tab. Closing a
/// panel with its own close button closes the sidebar. In turn the sidebar sets `app.right` to
/// the panel its tab shows, so the canvas renders thumbnails for Pages and hands the find bar's
/// search to Find, and `app.left_open` while the Tools tab shows a tool's options.
pub(crate) fn sync(app: &mut PdfCraftApp) {
    if app.active.is_none() {
        return;
    }
    let doc = app.active_ids().map(|(_, id)| id);
    let chrome = &mut app.margin_chrome;
    if doc != chrome.synced_doc {
        // PdfCraft opens a document on Comments or Bookmarks when it has them; a reader starts
        // on its contents. A panel the document asks for (Pages, Attachments…) still shows.
        chrome.synced_doc = doc;
        chrome.tab = match app.right {
            Some(RightPanel::Comments | RightPanel::Bookmarks) | None => SidebarTab::Contents,
            Some(panel) => SidebarTab::for_panel(panel),
        };
        chrome.synced_right = app.right;
    }
    if app.right != chrome.synced_right {
        match app.right {
            Some(panel) => {
                chrome.tab = SidebarTab::for_panel(panel);
                chrome.sidebar_open = true;
            }
            None => chrome.sidebar_open = false,
        }
    }
    let wanted = if chrome.sidebar_open { chrome.tab.panel() } else { None };
    chrome.synced_right = wanted;
    app.right = wanted;
    app.left_open = chrome.sidebar_open && chrome.tab == SidebarTab::Tools;
    app.comments_panel_closed = true;
}

/// Draw Margin's whole window (everything but full screen, which PdfCraft's `ui` handles).
pub(crate) fn ui(app: &mut PdfCraftApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    sync(app);
    let t = crate::theme::Tokens::get(ctx);
    if app.active.is_some() && app.margin_chrome.sidebar_open {
        sidebar::show(app, ui);
    }
    header::show(app, ui);
    if app.active.is_some() {
        footer::show(app, ui);
    }
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.pasteboard)).show(ui, |ui| match app.active {
        None if app.combine_showing() => crate::combine_ui::page(app, ui),
        None => crate::home::show(app, ui),
        Some(i) => crate::canvas::document_area(app, i, ui),
    });
    app.process_pending_edits();
    crate::palette::show(app, ctx);
    crate::dialogs::show(app, ctx);
    keys::show(app, ctx);
    app.show_progress(ctx);
    crate::widgets::toast(app, ctx);
}

/// The deepest outline entry at or before `page`, in reading order: the section being read.
/// Returns its index path. Bounded: at most [`MAX_OUTLINE_DEPTH`] levels and
/// [`MAX_OUTLINE_ITEMS`] entries are visited.
pub fn current_section(outline: &[pdfcraft_render::OutlineItem], page: usize) -> Option<Vec<usize>> {
    let mut best: Option<Vec<usize>> = None;
    let mut visited = 0usize;
    let mut stack: Vec<(&[pdfcraft_render::OutlineItem], usize)> = vec![(outline, 0)];
    let mut path: Vec<usize> = Vec::new();
    while let Some((items, next)) = stack.pop() {
        path.truncate(stack.len());
        let Some(item) = items.get(next) else { continue };
        visited += 1;
        if visited > MAX_OUTLINE_ITEMS {
            break;
        }
        stack.push((items, next + 1));
        path.push(next);
        if let Some(p) = item.page {
            // Entries are in reading order; one past the page ends the search at this level.
            if p > page {
                stack.pop();
                continue;
            }
            best = Some(path.clone());
        }
        if !item.children.is_empty() && stack.len() < MAX_OUTLINE_DEPTH {
            stack.push((&item.children, 0));
        }
    }
    best
}

/// The outline entry at `path`.
pub fn outline_at<'a>(outline: &'a [pdfcraft_render::OutlineItem], path: &[usize]) -> Option<&'a pdfcraft_render::OutlineItem> {
    let (first, rest) = path.split_first()?;
    let mut item = outline.get(*first)?;
    for i in rest {
        item = item.children.get(*i)?;
    }
    Some(item)
}

pub const MAX_OUTLINE_DEPTH: usize = 32;
pub const MAX_OUTLINE_ITEMS: usize = 20_000;

#[cfg(test)]
mod tests {
    use pdfcraft_render::OutlineItem;

    fn item(title: &str, page: Option<usize>, children: Vec<OutlineItem>) -> OutlineItem {
        OutlineItem { title: title.into(), page, children, open: false }
    }

    #[test]
    fn the_current_section_is_the_deepest_entry_at_or_before_the_page() {
        let outline = vec![
            item("1 Intro", Some(0), vec![]),
            item("2 Model", Some(2), vec![item("2.1 Tuples", Some(3), vec![]), item("2.2 Ops", Some(5), vec![])]),
            item("3 End", Some(8), vec![]),
        ];
        let title = |page| super::current_section(&outline, page).and_then(|p| super::outline_at(&outline, &p)).map(|i| i.title.clone());
        assert_eq!(title(0).as_deref(), Some("1 Intro"));
        assert_eq!(title(2).as_deref(), Some("2 Model"));
        assert_eq!(title(4).as_deref(), Some("2.1 Tuples"));
        assert_eq!(title(7).as_deref(), Some("2.2 Ops"));
        assert_eq!(title(9).as_deref(), Some("3 End"));
        assert_eq!(super::current_section(&[], 3), None);
    }

    #[test]
    fn entries_without_a_page_and_deep_outlines_are_bounded() {
        let mut deep = item("leaf", Some(1), vec![]);
        for _ in 0..200 {
            deep = item("level", None, vec![deep]);
        }
        let outline = vec![item("start", Some(0), vec![]), deep];
        let path = super::current_section(&outline, 5).unwrap();
        assert!(path.len() <= super::MAX_OUTLINE_DEPTH, "{}", path.len());
    }
}
