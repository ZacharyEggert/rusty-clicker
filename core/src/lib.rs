//! Cross-platform autoclicker core: a pure `ClickPlan` executed by a `Runner`
//! against injected seams (Clicker, Sleeper, Rng) so it's testable without a
//! real mouse or real time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rand::Rng;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Time between clicks. `Uniform` samples each gap in `[lo, hi]` for jitter.
#[derive(Clone, Copy, Debug)]
pub enum Interval {
    Fixed(Duration),
    Uniform(Duration, Duration),
}

impl Interval {
    fn next(&self, rng: &mut impl Rng) -> Duration {
        match *self {
            Interval::Fixed(d) => d,
            Interval::Uniform(lo, hi) => {
                if lo >= hi {
                    return lo;
                }
                Duration::from_nanos(rng.gen_range(lo.as_nanos()..=hi.as_nanos()) as u64)
            }
        }
    }
}

/// Stopping condition for a run.
#[derive(Clone, Copy, Debug)]
pub enum Repeat {
    Forever,
    Times(u64),
    For(Duration),
}

#[derive(Clone, Copy, Debug)]
pub enum Target {
    CurrentCursor,
    Fixed { x: i32, y: i32 },
}

/// Pure description of an autoclick run. No behavior, trivially constructed in tests.
#[derive(Clone, Copy, Debug)]
pub struct ClickPlan {
    pub button: MouseButton,
    pub interval: Interval,
    pub repeat: Repeat,
    pub target: Target,
}

/// Seam over OS input synthesis. Real impl wraps `enigo`; tests use `FakeClicker`.
pub trait Clicker {
    fn move_to(&mut self, x: i32, y: i32) -> Result<(), String>;
    fn click(&mut self, button: MouseButton) -> Result<(), String>;
}

/// Seam over time. Real impl sleeps; tests record without blocking.
pub trait Sleeper {
    fn sleep(&self, d: Duration);
}

/// Execute `plan`, polling `stop` between clicks. Returns the number of clicks performed.
pub fn run(
    plan: &ClickPlan,
    clicker: &mut impl Clicker,
    sleeper: &impl Sleeper,
    rng: &mut impl Rng,
    stop: &AtomicBool,
) -> Result<u64, String> {
    let mut elapsed = Duration::ZERO;
    let mut count: u64 = 0;

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        match plan.repeat {
            Repeat::Times(n) if count >= n => break,
            Repeat::For(limit) if elapsed >= limit => break,
            _ => {}
        }

        if let Target::Fixed { x, y } = plan.target {
            clicker.move_to(x, y)?;
        }
        clicker.click(plan.button)?;
        count += 1;

        // Stop before sleeping again if we've hit a count target.
        if let Repeat::Times(n) = plan.repeat {
            if count >= n {
                break;
            }
        }

        let gap = plan.interval.next(rng);
        sleeper.sleep(gap);
        elapsed += gap;
    }
    Ok(count)
}

// --- Real adapters -------------------------------------------------------

/// Sleeps with the OS thread sleep.
pub struct RealSleeper;
impl Sleeper for RealSleeper {
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// `enigo`-backed clicker. Owns the seam; enigo is the swappable adapter behind it.
pub struct EnigoClicker {
    enigo: enigo::Enigo,
}

impl EnigoClicker {
    pub fn new() -> Result<Self, String> {
        let enigo = enigo::Enigo::new(&enigo::Settings::default()).map_err(|e| e.to_string())?;
        Ok(Self { enigo })
    }
}

impl Clicker for EnigoClicker {
    fn move_to(&mut self, x: i32, y: i32) -> Result<(), String> {
        use enigo::Mouse;
        self.enigo
            .move_mouse(x, y, enigo::Coordinate::Abs)
            .map_err(|e| e.to_string())
    }

    fn click(&mut self, button: MouseButton) -> Result<(), String> {
        use enigo::Mouse;
        let b = match button {
            MouseButton::Left => enigo::Button::Left,
            MouseButton::Right => enigo::Button::Right,
            MouseButton::Middle => enigo::Button::Middle,
        };
        self.enigo
            .button(b, enigo::Direction::Click)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    #[derive(Default)]
    struct FakeClicker {
        clicks: u64,
        moves: Vec<(i32, i32)>,
        last_button: Option<MouseButton>,
    }
    impl Clicker for FakeClicker {
        fn move_to(&mut self, x: i32, y: i32) -> Result<(), String> {
            self.moves.push((x, y));
            Ok(())
        }
        fn click(&mut self, button: MouseButton) -> Result<(), String> {
            self.clicks += 1;
            self.last_button = Some(button);
            Ok(())
        }
    }

    struct FakeSleeper;
    impl Sleeper for FakeSleeper {
        fn sleep(&self, _d: Duration) {}
    }

    fn plan(repeat: Repeat) -> ClickPlan {
        ClickPlan {
            button: MouseButton::Left,
            interval: Interval::Fixed(Duration::from_millis(10)),
            repeat,
            target: Target::CurrentCursor,
        }
    }

    #[test]
    fn times_clicks_exactly_n() {
        let mut c = FakeClicker::default();
        let mut rng = StdRng::seed_from_u64(0);
        let stop = AtomicBool::new(false);
        let n = run(&plan(Repeat::Times(5)), &mut c, &FakeSleeper, &mut rng, &stop).unwrap();
        assert_eq!(n, 5);
        assert_eq!(c.clicks, 5);
    }

    #[test]
    fn stop_flag_halts_forever() {
        // Pre-set stop: a Forever plan must perform zero clicks.
        let mut c = FakeClicker::default();
        let mut rng = StdRng::seed_from_u64(0);
        let stop = AtomicBool::new(true);
        let n = run(&plan(Repeat::Forever), &mut c, &FakeSleeper, &mut rng, &stop).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn fixed_target_moves_before_each_click() {
        let mut c = FakeClicker::default();
        let mut rng = StdRng::seed_from_u64(0);
        let stop = AtomicBool::new(false);
        let p = ClickPlan {
            target: Target::Fixed { x: 100, y: 200 },
            ..plan(Repeat::Times(3))
        };
        run(&p, &mut c, &FakeSleeper, &mut rng, &stop).unwrap();
        assert_eq!(c.moves, vec![(100, 200); 3]);
    }

    #[test]
    fn uniform_interval_stays_in_bounds() {
        let mut rng = StdRng::seed_from_u64(42);
        let iv = Interval::Uniform(Duration::from_millis(5), Duration::from_millis(15));
        for _ in 0..1000 {
            let d = iv.next(&mut rng);
            assert!(d >= Duration::from_millis(5) && d <= Duration::from_millis(15));
        }
    }

    #[test]
    fn for_duration_stops_by_elapsed() {
        // interval 10ms, limit 25ms -> clicks at t=0,10,20 then elapsed=30>=25 stops.
        let mut c = FakeClicker::default();
        let mut rng = StdRng::seed_from_u64(0);
        let stop = AtomicBool::new(false);
        let n = run(
            &plan(Repeat::For(Duration::from_millis(25))),
            &mut c,
            &FakeSleeper,
            &mut rng,
            &stop,
        )
        .unwrap();
        assert_eq!(n, 3);
    }
}
