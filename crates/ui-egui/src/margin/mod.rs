//! Margin, the reader-first flavour of this UI ([`crate::Flavor::Margin`]). Everything here is
//! Margin-only; upstream files reach it through small hooks listed in Margin's `RESKIN-HOOKS.md`.
//!
//! The reader layout: a header with the document's title and section, a
//! left sidebar with tabs, a floating annotate rail over the page and a footer with the page and
//! zoom. Every control runs an existing PdfCraft command or panel; this module only arranges them.
//!
//! Margin's interface text is English for now: these strings are plain literals, not `tl!`, so
//! PdfCraft's catalogs (which must cover every `tl!` literal) stay complete.

pub mod contents;
pub mod display;
pub mod drop;
pub mod figures;
pub mod find;
pub mod focus;
pub mod footer;
pub mod header;
pub mod highlights;
pub mod history;
pub mod keys;
pub mod keysheet;
#[cfg(not(target_arch = "wasm32"))]
pub mod library;
pub mod look;
pub mod motion;
pub mod obsidian;
pub mod pagecolor;
pub mod pages;
pub mod rail;
pub mod reading;
pub mod saving;
pub mod scroll_step;
pub mod scrubber;
pub mod selection;
pub mod settings_watch;
pub mod sidebar;
#[cfg(not(target_arch = "wasm32"))]
pub mod stats;
#[cfg(not(target_arch = "wasm32"))]
pub mod stats_sheet;
pub mod toast;
pub mod tools;
pub mod trim;

use crate::{PdfCraftApp, RightPanel};

/// What the reader chrome shows. Whether the sidebar is open and its tab are saved with the app's
/// state ([`ReaderChrome::to_json`]); a first start takes them from the `ui.sidebar` settings.
/// The sidebar's width is the resizable panel's own, kept in egui's saved memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderChrome {
    pub sidebar_open: bool,
    pub tab: SidebarTab,
    /// The annotate rail floats over the page.
    pub rail_open: bool,
    /// The right panel the chrome last asked for. When `app.right` differs, a command or option
    /// picked a panel since, and the sidebar follows it.
    synced_right: Option<RightPanel>,
    /// The document shown when the chrome last synced.
    synced_doc: Option<pdfcraft_engine::DocId>,
    /// While a search is open: the tab and sidebar state it replaced, shown again when it closes
    /// (`find::sync`).
    before_find: Option<(SidebarTab, bool)>,
    /// Nothing was restored from a previous run: the first `sync` applies `ui.sidebar`.
    first_start: bool,
    /// The sidebar has been drawn once this run ([`Self::start_narrow`] decided).
    start_seen: bool,
    /// A narrow window closed the sidebar at start, not the reader: it is saved as open, and
    /// forgotten once the sidebar opens.
    closed_for_narrow_start: bool,
    /// Undo offers and held failures the toast shows (`margin::toast`); never saved.
    pub notices: toast::Notices,
}

impl Default for ReaderChrome {
    fn default() -> Self {
        Self {
            sidebar_open: true,
            tab: SidebarTab::Contents,
            rail_open: true,
            synced_right: None,
            synced_doc: None,
            before_find: None,
            first_start: true,
            start_seen: false,
            closed_for_narrow_start: false,
            notices: toast::Notices::default(),
        }
    }
}

impl ReaderChrome {
    /// The sidebar as the reader chose it, for the app's saved state. A search's Find tab is
    /// never saved: the tab and open state it replaced are. A PdfCraft panel without a tab saves
    /// no tab.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let (tab, open) = match self.before_find {
            Some(before) if self.tab == SidebarTab::Find => before,
            _ => (self.tab, self.sidebar_open),
        };
        let open = open || self.closed_for_narrow_start;
        serde_json::json!({ "sidebar_open": open, "tab": tab.key().filter(|_| tab != SidebarTab::Find) })
    }

    /// Called each time the sidebar is drawn. The first time, a window narrower than
    /// `ui.sidebar.overlay_below_px` (`narrow`) starts with it closed (CUSTOMIZATION's "closed
    /// at start"): open, its overlay would cover the page until dismissed.
    pub(crate) fn start_narrow(&mut self, narrow: bool) {
        if !self.start_seen {
            self.start_seen = true;
            if narrow && self.sidebar_open {
                self.sidebar_open = false;
                self.closed_for_narrow_start = true;
            }
        }
        if self.sidebar_open {
            self.closed_for_narrow_start = false;
        }
    }

    /// Restore what [`Self::to_json`] wrote. Malformed or missing values keep the defaults.
    pub(crate) fn restore(&mut self, value: &serde_json::Value) {
        let Some(open) = value["sidebar_open"].as_bool() else { return };
        self.sidebar_open = open;
        self.tab = value["tab"].as_str().and_then(SidebarTab::from_key).filter(|tab| *tab != SidebarTab::Find).unwrap_or(SidebarTab::Contents);
        self.first_start = false;
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

    /// The strip's tab whose [`Self::key`] is `key`.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::STRIP.into_iter().find(|tab| tab.key() == Some(key))
    }

    /// The tab the `ui.sidebar.tab` setting names.
    fn from_setting(tab: margin_config::SidebarTab) -> Self {
        use margin_config::SidebarTab as S;
        match tab {
            S::Contents => Self::Contents,
            S::Highlights => Self::Highlights,
            S::Pages => Self::Pages,
            S::Bookmarks => Self::Bookmarks,
            S::Tools => Self::Tools,
        }
    }
}

/// Margin's defaults on a new app (saved settings may override them afterwards).
pub(crate) fn apply_defaults(app: &mut PdfCraftApp) {
    // Picking a comment tool must not switch the sidebar to Highlights (see `sync`).
    app.comments_panel_closed = true;
}

/// Run a Margin-only action (the keymap's ids): `view.sidebar.toggle`, `view.sidebar.tab` with
/// `{"tab": "contents" | "highlights" | "pages" | "bookmarks" | "find" | "tools"}`,
/// `reader.annotate.toggle` (the rail), `reader.bookmark.toggle` (a page bookmark on the current
/// page), `view.trim.toggle`, and the library's `library.open`, `library.search` and `app.stats`.
/// Returns `false` for an unknown id or tab.
pub fn run(app: &mut PdfCraftApp, id: &str, args: &serde_json::Value) -> bool {
    match id {
        "view.sidebar.toggle" => app.margin_chrome.sidebar_open = !app.margin_chrome.sidebar_open,
        "reader.annotate.toggle" => app.margin_chrome.rail_open = !app.margin_chrome.rail_open,
        "view.sidebar.tab" => match args.get("tab").and_then(serde_json::Value::as_str).and_then(SidebarTab::from_key) {
            Some(tab) => open_tab(app, tab),
            None => return false,
        },
        // Without a document the key is spent, as PdfCraft's view keys are.
        "reader.bookmark.toggle" => {
            let Some(view) = app.active.and_then(|i| app.views.get_mut(i)) else { return true };
            let page = view.current + 1;
            let message =
                if reading::toggle_bookmark(view) { format!("Bookmarked page {page}") } else { format!("Removed the bookmark on page {page}") };
            app.notify(message);
        }
        "view.trim.toggle" => {
            let Some(view) = app.active.and_then(|i| app.views.get_mut(i)) else { return true };
            reading::toggle_trim(view);
        }
        highlights::DELETE_ID | highlights::COLOR_ID => return highlights::run(app, id, args),
        #[cfg(not(target_arch = "wasm32"))]
        "library.open" | "library.search" | "app.stats" => return library::run(app, id),
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
    if tab == SidebarTab::Find {
        // Find opens the search, remembering the tab to come back to.
        find::open(app);
        return;
    }
    let chrome = &mut app.margin_chrome;
    chrome.sidebar_open = true;
    chrome.tab = tab;
}

/// Keep PdfCraft's panel state and the sidebar in step, once per frame before drawing.
///
/// The first frame of a first start opens the sidebar as `ui.sidebar` says. Another document
/// keeps the reader's tab, unless its initial view asks for a panel (Pages, Attachments…).
/// Commands (`comment.list`, `edit.advanced_search`, `sign.panel`, the control channel's
/// `panel` option) still set `app.right`: the sidebar switches to the matching tab. Closing a
/// panel with its own close button closes the sidebar. In turn the sidebar sets `app.right` to
/// the panel its tab shows, so the canvas renders thumbnails for Pages, and `app.left_open`
/// while the Tools tab shows a tool's options. An open search shows the Find tab
/// (`find::sync`).
pub(crate) fn sync(app: &mut PdfCraftApp) {
    if app.margin_chrome.first_start {
        let configured = &app.margin_keys.settings().ui.sidebar;
        let chrome = &mut app.margin_chrome;
        chrome.first_start = false;
        chrome.sidebar_open = configured.visible;
        chrome.tab = SidebarTab::from_setting(configured.tab);
    }
    if app.active.is_none() {
        return;
    }
    let doc = app.active_ids().map(|(_, id)| id);
    let chrome = &mut app.margin_chrome;
    if doc != chrome.synced_doc {
        // PdfCraft opens a document on Comments or Bookmarks when it has them; that is not the
        // reader's choice, so the tab stays.
        chrome.synced_doc = doc;
        if let Some(panel) =
            app.right.filter(|panel| Some(*panel) != chrome.synced_right && !matches!(panel, RightPanel::Comments | RightPanel::Bookmarks))
        {
            chrome.tab = SidebarTab::for_panel(panel);
        }
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
    if find::sync(app) {
        sync(app);
    }
    motion::sync(ctx, app.margin_keys.settings().ui.reduce_motion);
    let t = crate::theme::Tokens::get(ctx);
    // A click while a popover or menu is open closes it (`look::popover`) and does nothing else:
    // the page must not also clear its selection, follow a link or place a mark.
    let dismissing = egui::Popup::is_any_open(ctx) && ctx.input(|i| i.pointer.any_click());
    // The sidebar slides itself in and out (`margin_chrome.sidebar_open`).
    if app.active.is_some() {
        sidebar::show(app, ui);
    }
    // The Library has its own single toolbar (with the app menu), so no reader header over it.
    let library_shown = cfg!(not(target_arch = "wasm32")) && app.active.is_none() && !app.combine_showing();
    if !library_shown && header::show(app, ui) {
        find::row(app, ui);
    }
    if app.active.is_some() {
        footer::show(app, ui);
    }
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.pasteboard)).show(ui, |ui| match app.active {
        None if app.combine_showing() => crate::combine_ui::page(app, ui),
        #[cfg(not(target_arch = "wasm32"))]
        None => library::show(app, ui),
        #[cfg(target_arch = "wasm32")]
        None => crate::home::show(app, ui),
        Some(i) => {
            #[cfg(not(target_arch = "wasm32"))]
            library::hidden(app);
            if dismissing {
                // Disabled for this frame only, at full opacity (as the canvas pauses a wheel
                // gesture).
                let opacity = ui.opacity();
                ui.disable();
                ui.set_opacity(opacity);
            }
            crate::canvas::document_area(app, i, ui);
            selection::show(app, ui.ctx(), i, ui.clip_rect());
        }
    });
    app.process_pending_edits();
    let pill_top = if app.active.is_some() { history::show(app, ctx) } else { None };
    drop::hint(app, ctx);
    crate::palette::show(app, ctx);
    crate::dialogs::show(app, ctx);
    keys::show(app, ctx);
    #[cfg(not(target_arch = "wasm32"))]
    if app.margin_stats.sheet_open() {
        stats_sheet::show(app, ctx);
    }
    app.show_progress(ctx);
    toast::show(app, ctx, pill_top);
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
