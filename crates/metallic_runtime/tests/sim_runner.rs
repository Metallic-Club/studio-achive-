//! Finite local-runner process tests, including the graphics-free configuration.
use metallic_core::{Value, World, save_scene};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Project(PathBuf);
impl Project {
    fn new(source: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "metallic-sim-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        fs::write(
            path.join("metallic.toml"),
            "name = 'Local fixture'\nmain_scene = 'main.mscene'\n",
        )
        .unwrap();
        let mut world = World::new_with_services();
        let script = world
            .create("Script", world.root("ServerScriptService"))
            .unwrap();
        world
            .set(script, "Source", Value::String(source.into()))
            .unwrap();
        save_scene(&world, &path.join("main.mscene")).unwrap();
        Self(path)
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_metallic-sim"));
    command.env("METALLIC_TEST_BUDGETS", "1");
    command
}

#[test]
fn finite_runner_executes_server_scripts_and_stops() {
    let project = Project::new(
        "assert(#game:GetService('Players'):GetChildren()==0) print('CPU_READY') task.delay(0.1,function() print('CPU_DELAYED') end)",
    );
    let output = command()
        .arg(&project.0)
        .args(["--ticks", "12"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        stdout.contains("CPU_READY") && stdout.contains("CPU_DELAYED"),
        "{stdout}"
    );
    assert!(
        stdout.contains("completed 12 ticks (0.200000 simulated seconds); stopped"),
        "{stdout}"
    );
    assert!(!stdout.contains("listening"));
}

#[test]
fn argument_load_and_script_failures_return_nonzero() {
    let project = Project::new("error('fixture failure')");
    let output = command()
        .arg(&project.0)
        .args(["--ticks", "1"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("fixture failure"));
    assert!(String::from_utf8_lossy(&output.stdout).contains("stopped"));
    let output = command()
        .arg(&project.0)
        .args(["--ticks", "0"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("stopped"));
    let output = command().arg(project.0.join("absent")).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let output = command()
        .arg(&project.0)
        .args(["--scene", "absent.mscene"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
}
