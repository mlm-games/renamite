//! Chainable tween sequences.
//!
//! A [`TweenBuilder`] compiles sequential / parallel steps, delays, callbacks
//! and a loop policy into a [`TweenSequence`] backed by the crate's
//! [`Tween`] values and [`EasingPreset`] easing. [`TweenPlayer`] advances the
//! sequence with `dt`, yields the sampled value, and reports callbacks crossed.

use crate::{EasingPreset, Tween, ease_progress};

/// How many times a sequence replays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LoopPolicy {
    #[default]
    Once,
    Forever,
    Times(u32),
}

impl LoopPolicy {
    /// Total sequence length for a single-step duration, or `None` forever.
    fn total(self, duration: f64) -> Option<f64> {
        match self {
            LoopPolicy::Once => Some(duration),
            LoopPolicy::Forever => None,
            LoopPolicy::Times(n) => Some(duration * n as f64),
        }
    }
}

#[derive(Clone, Debug)]
pub struct TweenStep<T: Tween> {
    pub start: f64,
    pub duration: f64,
    pub kind: TweenStepKind<T>,
}

#[derive(Clone, Debug)]
pub enum TweenStepKind<T: Tween> {
    Tween {
        from: T,
        to: T,
        easing: EasingPreset,
    },
    Callback(usize),
}

/// A compiled, samplable tween sequence.
#[derive(Clone, Debug)]
pub struct TweenSequence<T: Tween> {
    steps: Vec<TweenStep<T>>,
    duration: f64,
    loops: LoopPolicy,
}

impl<T: Tween> TweenSequence<T> {
    pub fn builder() -> TweenBuilder<T> {
        TweenBuilder::new()
    }

    /// Length of one pass through the sequence.
    pub fn duration(&self) -> f64 {
        self.duration
    }

    pub fn loops(&self) -> LoopPolicy {
        self.loops
    }

    pub fn steps(&self) -> &[TweenStep<T>] {
        &self.steps
    }

    /// Map an absolute player time onto this loop's `[0, duration]` window.
    fn phase(&self, time: f64) -> f64 {
        if !time.is_finite() || self.duration <= 0.0 {
            return 0.0;
        }
        match self.loops {
            LoopPolicy::Once => time.clamp(0.0, self.duration),
            LoopPolicy::Forever => time.rem_euclid(self.duration),
            LoopPolicy::Times(_) => match self.loops.total(self.duration) {
                Some(total) if time >= total => self.duration,
                _ => time.rem_euclid(self.duration),
            },
        }
    }

    /// Sample the animated value at absolute `time` (seconds).
    ///
    /// During a delay the previous tween's end value is held; before the first
    /// tween the first `from` is returned; an all-callback sequence yields
    /// `None`.
    pub fn sample(&self, time: f64) -> Option<T> {
        let t = self.phase(time);
        let mut covering: Option<(f64, &TweenStep<T>)> = None;
        let mut latest: Option<&TweenStep<T>> = None;
        let mut first_from: Option<&T> = None;
        for step in &self.steps {
            let TweenStepKind::Tween { from, .. } = &step.kind else {
                continue;
            };
            if first_from.is_none() {
                first_from = Some(from);
            }
            let end = step.start + step.duration;
            if step.start <= t && t <= end {
                if covering.is_none_or(|(start, _)| step.start >= start) {
                    covering = Some((step.start, step));
                }
            }
            if step.start <= t {
                latest = Some(step);
            }
        }
        if let Some((_, step)) = covering {
            return Some(self.value_of(step, t));
        }
        if let Some(step) = latest {
            return Some(self.value_of(step, step.start + step.duration));
        }
        first_from.cloned()
    }

    fn value_of(&self, step: &TweenStep<T>, time: f64) -> T {
        let TweenStepKind::Tween { from, to, easing } = &step.kind else {
            unreachable!("value_of only receives tween steps");
        };
        if step.duration <= 0.0 || !step.duration.is_finite() {
            return to.clone();
        }
        let u = ((time - step.start) / step.duration).clamp(0.0, 1.0);
        let (interpolation, ease_out, ease_in) = easing.segment();
        let y = ease_progress(interpolation, ease_out, ease_in, u);
        T::tween(from, to, y)
    }

    /// Occurrences of every callback step in `(lower, upper]`, in time order.
    fn callbacks_between(&self, lower: f64, upper: f64) -> Vec<usize> {
        let mut hits: Vec<(f64, usize)> = Vec::new();
        let passes = match self.loops {
            LoopPolicy::Once => 1,
            LoopPolicy::Times(n) => n as usize,
            LoopPolicy::Forever => {
                if self.duration <= 0.0 {
                    1
                } else {
                    ((upper / self.duration).floor() as usize).saturating_add(1)
                }
            }
        };
        for step in &self.steps {
            let TweenStepKind::Callback(id) = &step.kind else {
                continue;
            };
            for pass in 0..passes {
                let occurrence = pass as f64 * self.duration + step.start;
                if occurrence > lower && occurrence <= upper {
                    hits.push((occurrence, *id));
                }
            }
        }
        hits.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        hits.into_iter().map(|(_, id)| id).collect()
    }
}

/// Fluent builder for [`TweenSequence`].
///
/// Steps are sequential by default. [`TweenBuilder::parallel`] makes the
/// following steps start together with the previous one; [`TweenBuilder::sequential`]
/// returns to a sequential chain at the furthest end so far.
pub struct TweenBuilder<T: Tween> {
    steps: Vec<TweenStep<T>>,
    cursor: f64,
    last_start: f64,
    group_start: f64,
    parallel: bool,
    loops: LoopPolicy,
}

impl<T: Tween> Default for TweenBuilder<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Tween> TweenBuilder<T> {
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            cursor: 0.0,
            last_start: 0.0,
            group_start: 0.0,
            parallel: false,
            loops: LoopPolicy::Once,
        }
    }

    /// Append one eased tween step.
    pub fn tween(
        mut self,
        from: T,
        to: T,
        duration: f64,
        easing: EasingPreset,
    ) -> Self {
        let duration = finite_nonnegative(duration);
        let start = self.next_start();
        self.last_start = start;
        self.cursor = self.cursor.max(start + duration);
        self.steps.push(TweenStep {
            start,
            duration,
            kind: TweenStepKind::Tween { from, to, easing },
        });
        self
    }

    /// Append a zero-length callback step at the current cursor.
    pub fn callback(mut self, id: usize) -> Self {
        let start = self.next_start();
        self.last_start = start;
        self.steps.push(TweenStep {
            start,
            duration: 0.0,
            kind: TweenStepKind::Callback(id),
        });
        self
    }

    /// Advance the sequential cursor by `duration` without animating.
    pub fn delay(mut self, duration: f64) -> Self {
        let duration = finite_nonnegative(duration);
        let start = self.cursor;
        self.last_start = start;
        self.cursor = start + duration;
        self
    }

    /// The following steps run alongside the previous one.
    pub fn parallel(mut self) -> Self {
        self.group_start = self.last_start;
        self.parallel = true;
        self
    }

    /// Return to a sequential chain from the furthest end reached so far.
    pub fn sequential(mut self) -> Self {
        self.parallel = false;
        self
    }

    pub fn loops(mut self, loops: LoopPolicy) -> Self {
        self.loops = loops;
        self
    }

    pub fn build(self) -> TweenSequence<T> {
        TweenSequence {
            steps: self.steps,
            duration: self.cursor,
            loops: self.loops,
        }
    }

    fn next_start(&self) -> f64 {
        if self.parallel { self.group_start } else { self.cursor }
    }
}

/// Stateful driver for a [`TweenSequence`].
pub struct TweenPlayer<T: Tween> {
    sequence: TweenSequence<T>,
    time: f64,
    finished: bool,
}

impl<T: Tween> TweenPlayer<T> {
    pub fn new(sequence: TweenSequence<T>) -> Self {
        let finished = sequence.duration() <= 0.0 && matches!(sequence.loops(), LoopPolicy::Once);
        Self {
            sequence,
            time: 0.0,
            finished,
        }
    }

    pub fn from_builder(builder: TweenBuilder<T>) -> Self {
        Self::new(builder.build())
    }

    pub fn sequence(&self) -> &TweenSequence<T> {
        &self.sequence
    }

    pub fn time(&self) -> f64 {
        self.time
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn value(&self) -> Option<T> {
        self.sequence.sample(self.time)
    }

    pub fn restart(&mut self) {
        self.time = 0.0;
        self.finished =
            self.sequence.duration() <= 0.0 && matches!(self.sequence.loops(), LoopPolicy::Once);
    }

    /// Advance by `dt`, returning the callback ids crossed, in time order.
    pub fn advance(&mut self, dt: f64) -> Vec<usize> {
        if !dt.is_finite() || dt < 0.0 || self.finished {
            return Vec::new();
        }
        let old = self.time;
        let mut new = old + dt;
        if !new.is_finite() {
            return Vec::new();
        }
        if let Some(total) = self.sequence.loops.total(self.sequence.duration) {
            if new >= total {
                new = total;
                self.finished = true;
            }
        }
        let lower = if old == 0.0 && new > 0.0 { -f64::EPSILON } else { old };
        let fired = self.sequence.callbacks_between(lower, new);
        self.time = new;
        fired
    }
}

fn finite_nonnegative(value: f64) -> f64 {
    if value.is_finite() { value.max(0.0) } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequential_delay_and_hold() {
        let seq = TweenSequence::<f64>::builder()
            .tween(0.0, 1.0, 1.0, EasingPreset::Linear)
            .delay(0.5)
            .tween(1.0, 2.0, 0.5, EasingPreset::Linear)
            .build();
        assert_eq!(seq.duration(), 2.0);
        assert_eq!(seq.sample(0.5), Some(0.5));
        assert_eq!(seq.sample(1.25), Some(1.0));
        assert_eq!(seq.sample(1.75), Some(1.5));
        assert_eq!(seq.sample(5.0), Some(2.0));
    }

    #[test]
    fn parallel_steps_overlap_then_chain() {
        let seq = TweenSequence::<f64>::builder()
            .tween(0.0, 1.0, 1.0, EasingPreset::Linear)
            .parallel()
            .tween(10.0, 20.0, 2.0, EasingPreset::Linear)
            .sequential()
            .tween(0.0, 1.0, 1.0, EasingPreset::Linear)
            .build();
        assert_eq!(seq.duration(), 3.0);
        assert_eq!(seq.sample(1.5), Some(17.5));
        assert_eq!(seq.sample(2.5), Some(0.5));
    }

    #[test]
    fn callbacks_fire_once_in_time_order() {
        let seq = TweenSequence::<f64>::builder()
            .tween(0.0, 1.0, 0.5, EasingPreset::Linear)
            .callback(1)
            .delay(0.25)
            .callback(2)
            .build();
        let mut player = TweenPlayer::new(seq);
        assert_eq!(player.advance(1.0), vec![1, 2]);
        assert_eq!(player.advance(1.0), Vec::<usize>::new());
    }

    #[test]
    fn times_loop_finishes_after_n_passes() {
        let seq = TweenSequence::<f64>::builder()
            .tween(0.0, 1.0, 1.0, EasingPreset::Linear)
            .loops(LoopPolicy::Times(2))
            .build();
        let mut player = TweenPlayer::new(seq);
        player.advance(1.5);
        assert_eq!(player.value(), Some(0.5));
        assert!(!player.is_finished());
        player.advance(0.5);
        assert!(player.is_finished());
        assert_eq!(player.value(), Some(1.0));
    }

    #[test]
    fn forever_loop_wraps_and_replays_callbacks() {
        let seq = TweenSequence::<f64>::builder()
            .tween(0.0, 1.0, 1.0, EasingPreset::Linear)
            .callback(9)
            .loops(LoopPolicy::Forever)
            .build();
        let mut player = TweenPlayer::new(seq);
        assert_eq!(player.advance(2.5), vec![9, 9]);
        assert_eq!(player.value(), Some(0.5));
        assert!(!player.is_finished());
    }
}
