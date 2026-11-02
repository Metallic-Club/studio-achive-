//! End-to-end checks of `metallic-player` and `Game::load`.
use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The Player as a child process, on the test budgets (see `metallic_script`).
fn player() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_metallic-player"));
    command.env("METALLIC_TEST_BUDGETS", "1");
    command
}

/// Plays the Parkour template headless with the smoke scene: its test script teleports the
/// character into Checkpoint 2, kills it, checks the respawn at Checkpoint 2, touches the Finish
/// twice and checks exactly one `Finished` event arrived.
#[test]
fn parkour_smoke_scene_passes_headless() {
    let out = player()
        .arg(repo_root().join("templates/parkour"))
        .args([
            "--scene",
            "tests/smoke.mscene",
            "--headless",
            "--frames",
            "1200",
        ])
        .output()
        .expect("run metallic-player");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "exit {:?}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.status
    );
    assert!(
        stdout.contains("PARKOUR_SMOKE_OK"),
        "no PARKOUR_SMOKE_OK\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    for line in stdout.lines().chain(stderr.lines()) {
        assert!(
            !line.to_lowercase().contains("error"),
            "error line: {line}\nstdout:\n{stdout}\nstderr:\n{stderr}"
        );
    }
}

/// A project whose `main_scene` file does not exist fails to load with a clear error, and the
/// player exits with code 1.
#[test]
fn game_load_rejects_missing_main_scene() {
    let dir = std::env::temp_dir().join(format!("metallic_missing_scene_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(
        dir.join("metallic.toml"),
        "name = \"Broken\"\nmain_scene = \"nowhere.mscene\"\n",
    )
    .expect("write metallic.toml");

    let err = match metallic_runtime::Game::load(&dir) {
        Ok(_) => panic!("loading a project without its main scene must fail"),
        Err(e) => e,
    };
    assert!(
        matches!(err, metallic_runtime::RuntimeError::MissingScene(_)),
        "unexpected error: {err}"
    );
    assert!(err.to_string().contains("nowhere.mscene"), "{err}");

    let out = player()
        .arg(&dir)
        .args(["--headless", "--frames", "1"])
        .output()
        .expect("run metallic-player");
    assert_eq!(out.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--input-script` plays input events headless; a broken script is refused with its line number
/// and exit code 2.
#[test]
fn headless_run_plays_an_input_script() {
    let dir = std::env::temp_dir().join(format!("metallic_input_script_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let good = dir.join("walk.txt");
    std::fs::write(
        &good,
        "# walk forward, jump, stop\n10 keyboard W down\n20 keyboard Space down\n21 keyboard Space up\n\
         40 gamepad0 Thumbstick1 axis 0.0 1.0\n60 gamepad0 Thumbstick1 axis 0.0 0.0\n70 keyboard W up\n",
    )
    .expect("write script");
    let run = |script: &std::path::Path| {
        player()
            .arg(repo_root().join("templates/parkour"))
            .args(["--headless", "--frames", "120", "--input-script"])
            .arg(script)
            .output()
            .expect("run metallic-player")
    };
    let out = run(&good);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("ran 120 frames"));

    let bad = dir.join("bad.txt");
    std::fs::write(&bad, "10 keyboard Space down\n20 keyboard Spacebar down\n").expect("write");
    let out = run(&bad);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("line 2") && stderr.contains("Spacebar"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
