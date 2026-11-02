#![cfg(feature = "presentation")]
//! `workspace.CurrentCamera`, the projection methods and `Ray`, through a running `Game`.

use metallic_core::World;
use metallic_runtime::{FrameInput, Game, TICK};
use metallic_script::OutputSink;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct Captured(RefCell<Vec<String>>);

impl OutputSink for Captured {
    fn print(&self, line: &str) {
        self.0.borrow_mut().push(line.to_owned());
    }
    fn warn(&self, line: &str) {
        self.0.borrow_mut().push(format!("warning: {line}"));
    }
    fn error(&self, line: &str) {
        self.0.borrow_mut().push(format!("error: {line}"));
    }
}

/// A game running `source` as a server script, plus what it printed so far.
struct Run {
    game: Game,
    output: Rc<Captured>,
}

impl Run {
    fn new(source: &str) -> Run {
        let mut world = World::new_with_services();
        let script = world
            .create("Script", world.root("ServerScriptService"))
            .unwrap();
        world
            .set(
                script,
                "Source",
                metallic_core::Value::String(source.to_owned()),
            )
            .unwrap();
        let output = Rc::new(Captured::default());
        metallic_script::use_test_budgets_on_this_thread();
        let mut game = Game::from_world("camera-api".to_owned(), world, output.clone());
        game.start().unwrap();
        Run { game, output }
    }

    fn frames(&mut self, n: usize) -> metallic_runtime::FrameOutput {
        let mut last = None;
        for _ in 0..n {
            last = Some(self.game.frame(&FrameInput::default(), TICK));
        }
        last.expect("at least one frame")
    }

    fn lines(&self) -> Vec<String> {
        self.output.0.borrow().clone()
    }
}

fn run(source: &str) -> Vec<String> {
    let mut r = Run::new(source);
    r.frames(3);
    r.lines()
}

/// A scripted 90 degree camera at (0, 10, 0) looking along -Z, so the maths are easy to check.
const SET_UP: &str = "
local cam = workspace.CurrentCamera
cam.CameraType = Enum.CameraType.Scriptable
cam.CFrame = CFrame.new(0, 10, 0)
cam.FieldOfView = 90
local function near(a, b) return math.abs(a - b) < 1e-6 end
";

#[test]
fn the_current_camera_is_a_camera_with_the_games_pose() {
    let mut r = Run::new(
        "task.wait(0.1)
         local cam = workspace.CurrentCamera
         print(cam.ClassName, cam.Name, cam.Parent == workspace, cam == workspace.CurrentCamera)
         print(cam.CameraType == Enum.CameraType.Custom, cam.FieldOfView)
         print(cam.CFrame.Position.Magnitude > 1)",
    );
    let frame = r.frames(30);
    let lines = r.lines();
    assert_eq!(lines[0], "Camera Camera true true", "{lines:?}");
    assert_eq!(lines[1], "true 70", "{lines:?}");
    assert_eq!(lines[2], "true", "{lines:?}");
    // The Custom camera publishes itself: the script read a real pose, and the field of view
    // it reads is the one drawn.
    assert!((frame.camera.fov_deg - 70.0).abs() < 1e-3);
}

#[test]
fn a_scriptable_camera_is_the_drawn_camera_and_custom_takes_it_back() {
    let mut r = Run::new(
        "task.wait(0.3)
         local cam = workspace.CurrentCamera
         cam.CameraType = Enum.CameraType.Scriptable
         cam.CFrame = CFrame.lookAt(Vector3.new(10, 20, 30), Vector3.new(10, 20, 20))
         cam.FieldOfView = 40
         print(cam.CameraType == Enum.CameraType.Scriptable, cam.CameraType.Name)
         task.wait(0.5)
         cam.CameraType = 'Custom'
         print(cam.CameraType == Enum.CameraType.Custom)",
    );
    let custom = r.frames(10).camera;
    assert!((custom.fov_deg - 70.0).abs() < 1e-3);
    let scripted = r.frames(15).camera;
    assert!((scripted.position.x - 10.0).abs() < 1e-4);
    assert!((scripted.position.y - 20.0).abs() < 1e-4);
    assert!((scripted.position.z - 30.0).abs() < 1e-4);
    // It looks along -Z from there, and the field of view is the script's.
    assert!((scripted.look_at.z - scripted.position.z + 1.0).abs() < 1e-4);
    assert!((scripted.fov_deg - 40.0).abs() < 1e-4);
    let back = r.frames(60).camera;
    assert!((back.fov_deg - 70.0).abs() < 1e-3, "{back:?}");
    assert!(back.position != scripted.position);
    let lines = r.lines();
    assert_eq!(lines[0], "true Scriptable", "{lines:?}");
    assert_eq!(lines[1], "true", "{lines:?}");
}

#[test]
fn looking_straight_down_still_draws_a_valid_camera() {
    let mut r = Run::new(
        "task.wait(0.05)
         local cam = workspace.CurrentCamera
         cam.CameraType = Enum.CameraType.Scriptable
         cam.CFrame = CFrame.lookAt(Vector3.new(0, 50, 0), Vector3.new(0, 0, 0))",
    );
    r.frames(10);
    let c = r.frames(5).camera;
    assert!(c.position.is_finite() && c.look_at.is_finite());
    assert!(c.look_at.y < c.position.y);
}

#[test]
fn viewport_size_follows_what_the_host_reports() {
    let mut r = Run::new(
        "task.wait(0.05)
         local v = workspace.CurrentCamera.ViewportSize
         print(v.X, v.Y)
         task.wait(0.2)
         local w = workspace.CurrentCamera.ViewportSize
         print(w.X, w.Y)
         print(pcall(function() workspace.CurrentCamera.ViewportSize = Vector2.new(1, 1) end))",
    );
    r.frames(10);
    r.game.set_viewport_size(800.0, 600.0);
    r.game.set_viewport_size(f32::NAN, 100.0); // ignored
    r.frames(30);
    let lines = r.lines();
    assert_eq!(lines[0], "1280 720", "{lines:?}");
    assert_eq!(lines[1], "800 600", "{lines:?}");
    assert!(lines[2].starts_with("false"), "{lines:?}");
}

#[test]
fn viewport_points_become_rays_through_the_camera() {
    let mut r = Run::new(&format!(
        "{SET_UP}
         local function v(a) return string.format('%.4f %.4f %.4f', a.X, a.Y, a.Z) end
         task.wait(0.2)
         local c = cam:ViewportPointToRay(400, 300)
         print(v(c.Origin), v(c.Direction))
         local right = cam:ViewportPointToRay(800, 300)
         print(v(right.Direction))
         local top = cam:ViewportPointToRay(400, 0)
         print(v(top.Direction))
         local deep = cam:ViewportPointToRay(400, 300, 5)
         print(v(deep.Origin))
         print(near(right.Direction.Magnitude, 1), near(cam:ViewportPointToRay(13, 77).Direction.Magnitude, 1))"
    ));
    r.game.set_viewport_size(800.0, 600.0);
    r.frames(30);
    let lines = r.lines();
    // 90 degrees vertical: the top edge is 45 degrees up; the right edge is 53.13 degrees
    // out at the 4:3 aspect.
    assert_eq!(
        lines[0], "0.0000 10.0000 0.0000 0.0000 0.0000 -1.0000",
        "{lines:?}"
    );
    assert_eq!(lines[1], "0.8000 0.0000 -0.6000", "{lines:?}");
    assert_eq!(lines[2], "0.0000 0.7071 -0.7071", "{lines:?}");
    assert_eq!(lines[3], "0.0000 10.0000 -5.0000", "{lines:?}");
    assert_eq!(lines[4], "true true", "{lines:?}");
}

#[test]
fn world_points_land_on_the_viewport_and_round_trip_through_a_ray() {
    let mut r = Run::new(&format!(
        "{SET_UP}
         task.wait(0.2)
         local p, on = cam:WorldToViewportPoint(Vector3.new(0, 10, -10))
         print(p.X, p.Y, p.Z, on)
         local q, onq = cam:WorldToViewportPoint(Vector3.new(40 / 3, 10, -10))
         print(near(q.X, 800), near(q.Y, 300), near(q.Z, 10), onq)
         local behind, onb = cam:WorldToViewportPoint(Vector3.new(0, 10, 10))
         print(behind.Z < 0, onb)
         local far, onf = cam:WorldToViewportPoint(Vector3.new(100, 10, -10))
         print(far.X > 800, onf)
         -- a point goes to a pixel and the pixel's ray goes back through the point
         local world = Vector3.new(3, 14, -20)
         local px = cam:WorldToViewportPoint(world)
         local ray = cam:ViewportPointToRay(px.X, px.Y)
         print(Ray.new(ray.Origin, ray.Direction * 100):Distance(world) < 1e-6, ray.Direction.Z < 0)
         local s = cam:WorldToScreenPoint(world)
         print(near(s.X, px.X), near(s.Y, px.Y))"
    ));
    r.game.set_viewport_size(800.0, 600.0);
    r.frames(30);
    let lines = r.lines();
    assert_eq!(lines[0], "400 300 10 true", "{lines:?}");
    assert_eq!(lines[1], "true true true true", "{lines:?}");
    assert_eq!(lines[2], "true false", "{lines:?}");
    assert_eq!(lines[3], "true false", "{lines:?}");
    assert_eq!(lines[4], "true true", "{lines:?}");
    assert_eq!(lines[5], "true true", "{lines:?}");
}

#[test]
fn screen_points_are_measured_below_the_gui_inset() {
    let mut r = Run::new(&format!(
        "{SET_UP}
         task.wait(0.2)
         local a = cam:ScreenPointToRay(400, 264)
         local b = cam:ViewportPointToRay(400, 300)
         print(near(a.Direction.X, b.Direction.X), near(a.Direction.Y, b.Direction.Y), near(a.Direction.Z, b.Direction.Z))
         local s = cam:WorldToScreenPoint(Vector3.new(0, 10, -10))
         local v = cam:WorldToViewportPoint(Vector3.new(0, 10, -10))
         print(near(s.Y, v.Y - 36), near(s.X, v.X))"
    ));
    r.game.set_viewport_size(800.0, 600.0);
    r.game.set_gui_inset(36.0);
    r.frames(30);
    let lines = r.lines();
    assert_eq!(lines[0], "true true true", "{lines:?}");
    assert_eq!(lines[1], "true true", "{lines:?}");
}

#[test]
fn the_camera_is_rotated_by_its_cframe() {
    let lines = run(&format!(
        "{SET_UP}
         cam.CFrame = CFrame.lookAt(Vector3.new(0, 10, 0), Vector3.new(10, 10, 0))
         local c = cam:ViewportPointToRay(640, 360)
         print(near(c.Direction.X, 1), near(c.Direction.Z, 0))
         local p, on = cam:WorldToViewportPoint(Vector3.new(10, 10, 0))
         print(near(p.X, 640), near(p.Y, 360), on)
         -- a field of view of 120 sees more than 40
         cam.FieldOfView = 40
         local _, narrow = cam:WorldToViewportPoint(Vector3.new(10, 10, 8))
         cam.FieldOfView = 120
         local _, wide = cam:WorldToViewportPoint(Vector3.new(10, 10, 8))
         print(narrow, wide)"
    ));
    assert_eq!(lines[0], "true true", "{lines:?}");
    assert_eq!(lines[1], "true true true", "{lines:?}");
    assert_eq!(lines[2], "false true", "{lines:?}");
}

#[test]
fn ray_is_a_datatype() {
    let lines = run(
        "local r = Ray.new(Vector3.new(1, 2, 3), Vector3.new(0, 0, -4))
         print(r.Origin, r.Direction, r.Unit.Direction, r.Unit.Origin)
         print(r:ClosestPoint(Vector3.new(5, 2, -1)), r:Distance(Vector3.new(5, 2, -1)))
         print(r:ClosestPoint(Vector3.new(0, 0, 100)), r:ClosestPoint(Vector3.new(0, 0, -100)))
         print(r == Ray.new(Vector3.new(1, 2, 3), Vector3.new(0, 0, -4)), r == Ray.new(Vector3.new(), Vector3.new(0, 0, -4)))
         print(typeof(r), tostring(r))
         print(pcall(Ray.new, 1, 2))
         print(pcall(function() r.Origin = Vector3.new() end))",
    );
    assert_eq!(lines[0], "1, 2, 3 0, 0, -4 0, 0, -1 1, 2, 3", "{lines:?}");
    assert_eq!(lines[1], "1, 2, -1 4", "{lines:?}");
    assert_eq!(lines[2], "1, 2, 3 1, 2, -1", "{lines:?}");
    assert_eq!(lines[3], "true false", "{lines:?}");
    assert_eq!(lines[4], "Ray {1, 2, 3}, {0, 0, -4}", "{lines:?}");
    assert!(lines[5].starts_with("false"), "{lines:?}");
    assert!(lines[6].starts_with("false"), "{lines:?}");
}

#[test]
fn camera_properties_and_methods_check_their_arguments() {
    let lines = run("local cam = workspace.CurrentCamera
         print(pcall(function() cam.CameraType = 'Orbital' end))
         print(pcall(function() cam.CameraType = Enum.Material.Glass end))
         print(pcall(function() cam.FieldOfView = 500 end), cam.FieldOfView)
         print(pcall(function() workspace.CurrentCamera = cam end))
         print(pcall(cam.ViewportPointToRay, cam, 'x', 1))
         print(pcall(cam.ViewportPointToRay, cam, 1, 1/0))
         print(pcall(cam.WorldToViewportPoint, cam, 5))
         print(pcall(function() return workspace.Camera.Name end))
         print(pcall(function() return workspace.ViewportPointToRay end))");
    for (i, l) in lines.iter().take(2).enumerate() {
        assert!(l.starts_with("false"), "line {i}: {lines:?}");
    }
    // Out-of-range field of view is clamped, not an error.
    assert!(lines[2].starts_with("true 120"), "{lines:?}");
    assert!(lines[3].starts_with("false"), "{lines:?}");
    for (i, l) in lines.iter().enumerate().take(7).skip(4) {
        assert!(l.starts_with("false"), "line {i}: {lines:?}");
    }
    assert!(lines[7].starts_with("true Camera"), "{lines:?}");
    assert!(lines[8].starts_with("false"), "{lines:?}");
}
