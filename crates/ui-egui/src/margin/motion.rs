//! Margin's motion: how long each kind of change takes and how it eases, in one place. Every
//! animation in Margin's chrome goes through here, on egui's own animation manager
//! (`Context::animate_bool_with_time_and_easing`, `animate_value_with_time`), which repaints only
//! while a value is still moving, so an idle window stays idle.
//!
//! `ui.reduce_motion` in `settings.jsonc` sets every duration to zero (egui's own fades too).
//! Page rendering is never animated, nor is a page colour switch (a crossfade would keep two
//! textures per page on screen). Of scrolling, only a screen step moves over time
//! (`margin::scroll_step`, [`STEP`]); the wheel, touch and drags scroll as they come.

use egui::{Context, Id};

/// Hover fills on rows, buttons and chips.
pub const HOVER: f32 = 0.08;
/// Things that appear in place: popovers, tooltips, switches, the full-screen bar.
pub const FADE: f32 = 0.12;
/// Things that slide in from an edge: the sidebar, the rail; and progress bars catching up.
pub const SLIDE: f32 = 0.16;
/// How far a popover or bar travels while it fades in (the mockup's `popin`: 4 px).
pub const RISE: f32 = 4.0;
/// How far the rail travels from the window edge while it fades in.
pub const RAIL_SHIFT: f32 = 12.0;
/// A screen step (Space, Page Down) gliding to its place.
pub const STEP: f32 = 0.18;

/// Ease out: quick to respond, gentle to land. egui mirrors it on the way back.
pub(crate) fn ease(t: f32) -> f32 {
    egui::emath::easing::cubic_out(t)
}

fn reduced_id() -> Id {
    Id::new("margin-reduce-motion")
}

/// Apply `ui.reduce_motion` for this frame: Margin's durations and egui's own fade time (popups
/// and tooltips fade in over `Style::animation_time`). Call once per frame before drawing.
pub(crate) fn sync(ctx: &Context, reduce: bool) {
    ctx.data_mut(|d| d.insert_temp(reduced_id(), reduce));
    let fade = if reduce { 0.0 } else { FADE };
    if ctx.global_style().animation_time != fade {
        ctx.global_style_mut(|style| style.animation_time = fade);
    }
}

/// `duration`, or zero when motion is reduced.
fn time(ctx: &Context, duration: f32) -> f32 {
    if reduced(ctx) { 0.0 } else { duration }
}

/// Whether `ui.reduce_motion` is on this frame.
pub(crate) fn reduced(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(reduced_id())).unwrap_or(false)
}

/// 0 → 1 as `on` turns true over `duration`, eased; back to 0 when it turns false. The first
/// call returns the end value at once (nothing animates in on the first frame).
pub(crate) fn toggle(ctx: &Context, id: Id, on: bool, duration: f32) -> f32 {
    ctx.animate_bool_with_time_and_easing(id, on, time(ctx, duration), ease)
}

/// A number that follows `target` over `duration` (linearly), such as a progress bar's fill.
pub(crate) fn follow(ctx: &Context, id: Id, target: f32, duration: f32) -> f32 {
    ctx.animate_value_with_time(id, target, time(ctx, duration))
}

/// How much of a hover fill to show for the widget `id` (0 → 1 over [`HOVER`]).
pub(crate) fn hover(ctx: &Context, id: Id, hovered: bool) -> f32 {
    toggle(ctx, id.with("margin-hover"), hovered, HOVER)
}
