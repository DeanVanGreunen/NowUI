//! Drives `transition-*`/`duration-*`/`ease-*`/`delay-*` classes: interpolates
//! the animatable subset of a node's style (colors, opacity, radius, 2D
//! transform — see `nowui_core::AnimatableStyle`) whenever its effective
//! style target changes (e.g. a `hover:` variant activates), rather than
//! snapping instantly.
//!
//! This is deliberately time-driven, not a continuous animation loop: a
//! redraw is requested every frame ONLY while at least one transition is
//! in-flight (see `any_active`); once all finish, the app harness goes back
//! to `ControlFlow::Wait` with no further redraws, per CLAUDE.md's
//! event-driven rendering rule.

use std::collections::HashMap;
use std::time::Instant;

use nowui_core::{AnimatableStyle, Easing, NodeId, Transition};

struct Anim {
    from: AnimatableStyle,
    to: AnimatableStyle,
    start: Instant,
    duration_ms: f32,
    delay_ms: f32,
    easing: Easing,
}

impl Anim {
    fn progress(&self, now: Instant) -> f32 {
        let elapsed_ms = now.duration_since(self.start).as_secs_f32() * 1000.0;
        if elapsed_ms <= self.delay_ms {
            return 0.0;
        }
        if self.duration_ms <= 0.0 {
            return 1.0;
        }
        let t = (elapsed_ms - self.delay_ms) / self.duration_ms;
        self.easing.apply(t.clamp(0.0, 1.0))
    }

    fn finished(&self, now: Instant) -> bool {
        let elapsed_ms = now.duration_since(self.start).as_secs_f32() * 1000.0;
        elapsed_ms >= self.delay_ms + self.duration_ms
    }

    fn current(&self, now: Instant) -> AnimatableStyle {
        AnimatableStyle::lerp(self.from, self.to, self.progress(now))
    }
}

#[derive(Default)]
pub struct Transitions {
    anims: HashMap<NodeId, Anim>,
}

impl Transitions {
    pub fn new() -> Self {
        Self::default()
    }

    /// True if any node is still mid-transition — the caller should keep
    /// requesting redraws while this holds, and stop once it doesn't.
    pub fn any_active(&self, now: Instant) -> bool {
        self.anims.values().any(|a| !a.finished(now))
    }

    /// Advance (or start) `id`'s transition toward `target` and return this
    /// frame's interpolated `AnimatableStyle`. `spec` is `None` when the node
    /// has no `transition-*` class, in which case this snaps instantly and
    /// forgets any prior animation state for `id`.
    pub fn step(&mut self, id: NodeId, target: AnimatableStyle, spec: Option<Transition>, now: Instant) -> AnimatableStyle {
        let Some(spec) = spec else {
            self.anims.remove(&id);
            return target;
        };

        match self.anims.get_mut(&id) {
            Some(anim) if anim.to == target => anim.current(now),
            Some(anim) => {
                // Retarget mid-flight: animate from wherever we currently are,
                // not from `anim.from`, so reversing direction doesn't jump.
                let current = anim.current(now);
                *anim = Anim {
                    from: current,
                    to: target,
                    start: now,
                    duration_ms: spec.duration_ms,
                    delay_ms: spec.delay_ms,
                    easing: spec.easing,
                };
                current
            }
            None => {
                // First time seeing this node: no prior value to animate
                // from, so start at rest (no animate-in from nothing).
                self.anims.insert(
                    id,
                    Anim { from: target, to: target, start: now, duration_ms: 0.0, delay_ms: 0.0, easing: Easing::Linear },
                );
                target
            }
        }
    }
}
