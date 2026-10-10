//! Margin's screen steps: Space/Shift+Space and Page Down/Page Up move the page by
//! `view.scroll_screen` of the window (the rest stays on screen, so the next line is easy to
//! find), gliding over [`motion::STEP`] when `view.smooth_scroll` is on and at once under
//! `ui.reduce_motion`. The canvas still scrolls by `DocView::key_scroll`; [`drive`] adds each
//! frame's share to it.
//!
//! Steps never queue: a new step first lands the one in flight, so held keys move exactly a
//! screen per press, and a wheel turn, a touch or a pointer press drops what is left, so the
//! reader's own scrolling never fights an animation. In Single a step pans an enlarged page to
//! its edge before the page turns ([`single_pan`]). Continuous scrolling never snaps to pages.

use egui::{Context, Event};

use super::motion;
use crate::{DocView, PdfCraftApp};

/// A page edge closer than this to the window's edge is in view.
const EDGE: f32 = 1.0;

/// One document view's screen step.
#[derive(Default)]
pub(crate) struct ScreenStep {
    /// Points asked for since the last frame (down; negative is up).
    requested: f32,
    gliding: Option<Glide>,
}

/// A step under way.
#[derive(Clone, Copy)]
struct Glide {
    start: f64,
    distance: f32,
    /// How much of `distance` the canvas has been given.
    given: f32,
}

impl ScreenStep {
    /// Step `by` points (negative: up).
    pub(crate) fn request(&mut self, by: f32) {
        self.requested += by;
    }

    /// Drops the step under way and any asked for: the view jumped, zoomed or turned a page.
    pub(crate) fn stop(&mut self) {
        *self = Self::default();
    }

    fn is_moving(&self) -> bool {
        self.gliding.is_some() || self.requested != 0.0
    }

    /// The points to scroll this frame, at `now`. `animate` is `view.smooth_scroll` without
    /// reduced motion; `interrupted` is the reader's own scrolling or a press this frame.
    fn advance(&mut self, now: f64, animate: bool, interrupted: bool) -> f32 {
        if interrupted {
            self.gliding = None;
        }
        let mut by = 0.0;
        let requested = std::mem::take(&mut self.requested);
        if requested != 0.0 {
            if let Some(glide) = self.gliding.take() {
                by += glide.distance - glide.given;
            }
            if animate {
                self.gliding = Some(Glide { start: now, distance: requested, given: 0.0 });
            } else {
                by += requested;
            }
        }
        if let Some(glide) = &mut self.gliding {
            let progress = ((now - glide.start) / f64::from(motion::STEP)).clamp(0.0, 1.0) as f32;
            let reached = glide.distance * motion::ease(progress);
            by += reached - glide.given;
            glide.given = reached;
            if progress >= 1.0 {
                self.gliding = None;
            }
        }
        by
    }
}

/// Every Margin frame, after the keys and before the page is drawn: each view's share of its
/// screen step, into `key_scroll`. Asks for the next frame only while a step is moving.
pub(crate) fn drive(app: &mut PdfCraftApp, ctx: &Context) {
    let animate = app.margin_keys.settings().view.smooth_scroll && !motion::reduced(ctx);
    let (now, interrupted) = ctx.input(|i| {
        let scrolled = i.events.iter().any(|event| matches!(event, Event::MouseWheel { .. } | Event::Touch { .. }));
        (i.time, scrolled || i.pointer.any_pressed())
    });
    for view in &mut app.views {
        let step = &mut view.margin.screen_step;
        if !step.is_moving() {
            continue;
        }
        view.key_scroll += step.advance(now, animate, interrupted);
        if step.is_moving() {
            ctx.request_repaint();
        }
    }
}

/// In Single, the pan a screen step of `by` points makes on an enlarged page: the step, cut at
/// the page's edge. `None` when that edge is already in the window, so the step turns the page.
pub(crate) fn single_pan(view: &DocView, by: f32) -> Option<f32> {
    let page = view.page_screen_rect(view.current)?;
    let window = view.viewport_rect();
    if by > 0.0 {
        let below = page.bottom() - window.bottom();
        (below > EDGE).then(|| by.min(below))
    } else {
        let above = window.top() - page.top();
        (above > EDGE).then(|| by.max(-above))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: f32 = 600.0;
    const FRAME: f64 = 1.0 / 60.0;

    /// Frames from `start` until the step settles, with the points given out.
    fn settle(step: &mut ScreenStep, start: f64, animate: bool) -> (f32, usize) {
        let (mut moved, mut frames, mut now) = (0.0, 0, start);
        while step.is_moving() {
            moved += step.advance(now, animate, false);
            frames += 1;
            now += FRAME;
            assert!(frames < 100, "a step ends");
        }
        (moved, frames)
    }

    #[test]
    fn a_step_glides_the_whole_screen_and_ends() {
        let mut step = ScreenStep::default();
        step.request(SCREEN);
        let first = step.advance(0.0, true, false);
        assert!(first.abs() < 1e-3, "it starts from where it is: {first}");
        let (moved, frames) = settle(&mut step, FRAME, true);
        assert!((moved - SCREEN).abs() < 1e-3, "{moved}");
        let expected = (f64::from(motion::STEP) / FRAME).ceil() as usize;
        assert!(frames.abs_diff(expected) <= 1, "{frames} frames");
    }

    #[test]
    fn without_animation_a_step_lands_at_once() {
        let mut step = ScreenStep::default();
        step.request(-SCREEN);
        assert_eq!(step.advance(0.0, false, false), -SCREEN);
        assert!(!step.is_moving());
    }

    #[test]
    fn held_steps_add_up_exactly_without_queueing() {
        let mut step = ScreenStep::default();
        let mut moved = 0.0;
        for press in 0..5 {
            step.request(SCREEN);
            moved += step.advance(f64::from(press) * FRAME * 2.0, true, false);
            assert!(step.gliding.is_some_and(|glide| glide.distance == SCREEN), "one screen in flight, never more");
        }
        moved += settle(&mut step, 10.0 * FRAME, true).0;
        assert!((moved - 5.0 * SCREEN).abs() < 1e-2, "{moved}");
    }

    #[test]
    fn the_readers_own_scrolling_drops_the_rest() {
        let mut step = ScreenStep::default();
        step.request(SCREEN);
        let mut moved = step.advance(0.0, true, false) + step.advance(FRAME, true, false);
        moved += step.advance(2.0 * FRAME, true, true);
        assert!(!step.is_moving());
        assert!(moved > 0.0 && moved < SCREEN, "{moved}");
    }
}
