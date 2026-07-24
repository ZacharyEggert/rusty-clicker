use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, ValueEnum};
use rusty_clicker_core::{
    run, ClickPlan, EnigoClicker, Interval, MouseButton, RealSleeper, Repeat, Target,
};

#[derive(Copy, Clone, Debug, ValueEnum)]
enum Button {
    Left,
    Right,
    Middle,
}

impl From<Button> for MouseButton {
    fn from(b: Button) -> Self {
        match b {
            Button::Left => MouseButton::Left,
            Button::Right => MouseButton::Right,
            Button::Middle => MouseButton::Middle,
        }
    }
}

/// Cross-platform autoclicker. Ctrl-C stops.
#[derive(Parser, Debug)]
#[command(name = "rusty-clicker", version)]
struct Cli {
    /// Milliseconds between clicks.
    #[arg(long, default_value_t = 100)]
    interval: u64,

    /// Jitter upper bound (ms); interval becomes uniform [interval, jitter-max].
    #[arg(long)]
    jitter_max: Option<u64>,

    /// Mouse button.
    #[arg(long, value_enum, default_value_t = Button::Left)]
    button: Button,

    /// Stop after N clicks. Omit for infinite (until Ctrl-C).
    #[arg(long)]
    times: Option<u64>,

    /// Stop after N seconds. Mutually exclusive with --times.
    #[arg(long)]
    duration: Option<u64>,

    /// Click at fixed X,Y instead of the current cursor. Format: X,Y
    #[arg(long, value_parser = parse_xy)]
    at: Option<(i32, i32)>,
}

fn parse_xy(s: &str) -> Result<(i32, i32), String> {
    let (x, y) = s.split_once(',').ok_or("expected X,Y")?;
    Ok((
        x.trim().parse().map_err(|_| "bad X")?,
        y.trim().parse().map_err(|_| "bad Y")?,
    ))
}

fn build_plan(cli: &Cli) -> Result<ClickPlan> {
    if cli.times.is_some() && cli.duration.is_some() {
        bail!("--times and --duration are mutually exclusive");
    }
    let interval = match cli.jitter_max {
        Some(hi) if hi >= cli.interval => {
            Interval::Uniform(Duration::from_millis(cli.interval), Duration::from_millis(hi))
        }
        Some(_) => bail!("--jitter-max must be >= --interval"),
        None => Interval::Fixed(Duration::from_millis(cli.interval)),
    };
    let repeat = match (cli.times, cli.duration) {
        (Some(n), _) => Repeat::Times(n),
        (_, Some(s)) => Repeat::For(Duration::from_secs(s)),
        _ => Repeat::Forever,
    };
    let target = match cli.at {
        Some((x, y)) => Target::Fixed { x, y },
        None => Target::CurrentCursor,
    };
    Ok(ClickPlan {
        button: cli.button.into(),
        interval,
        repeat,
        target,
    })
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let plan = build_plan(&cli)?;

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed))
            .context("installing Ctrl-C handler")?;
    }

    let mut clicker = EnigoClicker::new().map_err(|e| anyhow::anyhow!(e))?;
    let mut rng = rand::thread_rng();

    let n = run(&plan, &mut clicker, &RealSleeper, &mut rng, &stop).map_err(|e| anyhow::anyhow!(e))?;
    println!("{n} clicks");
    Ok(())
}
