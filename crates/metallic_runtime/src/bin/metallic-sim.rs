//! Finite trusted-project CPU simulation. No listener, network admission or hosting.
use metallic_runtime::Simulation;
use metallic_script::OutputSink;
use std::{cell::Cell, path::PathBuf, process::ExitCode, rc::Rc};

const USAGE: &str = "metallic-sim <trusted-project> [--scene FILE] [--ticks N]\n\
Local simulation only; no listener or player admission. N is 1..=1000000 (default 600).";

struct Arguments {
    project: PathBuf,
    scene: Option<PathBuf>,
    ticks: u64,
}

fn parse(args: impl IntoIterator<Item = String>) -> Result<Option<Arguments>, String> {
    let mut args = args.into_iter();
    let mut project = None;
    let mut scene = None;
    let mut ticks = 600;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--help" | "-h" => return Ok(None),
            "--scene" => scene = Some(PathBuf::from(args.next().ok_or("--scene needs a value")?)),
            "--ticks" => {
                ticks = args
                    .next()
                    .ok_or("--ticks needs a value")?
                    .parse::<u64>()
                    .map_err(|_| "--ticks must be an integer in 1..=1000000")?;
                if !(1..=1_000_000).contains(&ticks) {
                    return Err("--ticks must be in 1..=1000000".into());
                }
            }
            value if value.starts_with('-') => return Err(format!("unknown option {value}")),
            value if project.is_none() => project = Some(PathBuf::from(value)),
            _ => return Err("unexpected extra argument".into()),
        }
    }
    Ok(Some(Arguments {
        project: project.ok_or("missing trusted project")?,
        scene,
        ticks,
    }))
}

#[derive(Default)]
struct Terminal {
    failed: Cell<bool>,
}
impl OutputSink for Terminal {
    fn print(&self, line: &str) {
        println!("{line}");
    }
    fn warn(&self, line: &str) {
        eprintln!("warning: {line}");
    }
    fn error(&self, line: &str) {
        self.failed.set(true);
        eprintln!("error: {line}");
    }
}

fn main() -> ExitCode {
    metallic_script::use_test_budgets_if_a_test_asked();
    let arguments = match parse(std::env::args().skip(1)) {
        Ok(Some(arguments)) => arguments,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("{error}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let output = Rc::new(Terminal::default());
    let mut simulation = match Simulation::open(
        &arguments.project,
        arguments.scene.as_deref(),
        output.clone(),
    ) {
        Ok(simulation) => simulation,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = simulation.start() {
        simulation.stop();
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    let mut completed = true;
    for _ in 0..arguments.ticks {
        if !simulation.tick_once(&[]) {
            completed = false;
            break;
        }
    }
    simulation.stop();
    println!(
        "local simulation: {} completed {} ticks ({:.6} simulated seconds); stopped",
        simulation.name(),
        simulation.tick_count(),
        simulation.time()
    );
    if completed && !output.failed.get() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(text: &str) -> Result<Option<Arguments>, String> {
        parse(text.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn finite_limits_and_missing_values_are_checked_before_load() {
        assert!(arguments("place --ticks 1").is_ok());
        assert_eq!(
            arguments("place --ticks 1000000").unwrap().unwrap().ticks,
            1_000_000
        );
        for text in [
            "",
            "place --ticks",
            "place --ticks 0",
            "place --ticks -1",
            "place --ticks 1000001",
            "place --ticks NaN",
            "place --scene",
            "place extra",
            "place --listen",
        ] {
            assert!(arguments(text).is_err(), "{text}");
        }
        assert!(arguments("--help").unwrap().is_none());
    }
}
