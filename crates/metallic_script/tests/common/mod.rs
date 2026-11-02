//! Shared test rig: a world with the services, a host and a capture of all output.
#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use metallic_core::{InstanceId, Value, World};
use metallic_script::{OutputSink, ScriptHost};

/// Records every line scripts print (`print:`), warn (`warn:`) or fail with (`error:`).
#[derive(Default)]
pub struct Capture {
    lines: RefCell<Vec<String>>,
}

impl OutputSink for Capture {
    fn print(&self, line: &str) {
        self.lines.borrow_mut().push(format!("print:{line}"));
    }
    fn warn(&self, line: &str) {
        self.lines.borrow_mut().push(format!("warn:{line}"));
    }
    fn error(&self, line: &str) {
        self.lines.borrow_mut().push(format!("error:{line}"));
    }
}

/// Runs a test of the real wall-clock limits, up to five times until one run passes.
///
/// Only these tests may do this: they cannot use the test budgets, because the real time is what
/// they check. A stall of the machine fails one run; a broken limit fails every run. The body
/// must build its own VM or rig, and the last run's panic is the test's failure.
pub fn real_limits_test<T>(mut body: impl FnMut() -> T) -> T {
    for _ in 1..5 {
        if let Ok(out) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(&mut body)) {
            return out;
        }
    }
    body()
}

pub struct Rig {
    pub world: Rc<RefCell<World>>,
    pub host: ScriptHost,
    pub out: Rc<Capture>,
}

impl Rig {
    pub fn new() -> Rig {
        let world = Rc::new(RefCell::new(World::new_with_services()));
        let out = Rc::new(Capture::default());
        // The budgets are wall-clock time and a busy machine can stall a trivial script past them.
        // Tests of the limits set `RESUME_BUDGET` or `STEP_BUDGET_SEC` back on the host.
        metallic_script::use_test_budgets_on_this_thread();
        let host = ScriptHost::new(world.clone(), out.clone());
        Rig { world, host, out }
    }

    pub fn root(&self, class: &str) -> InstanceId {
        self.world.borrow().root(class).expect("service exists")
    }

    /// Creates an instance of `class` named `name` under `parent`.
    pub fn add(&self, class: &str, name: &str, parent: InstanceId) -> InstanceId {
        let mut w = self.world.borrow_mut();
        let id = w.create(class, Some(parent)).expect("create");
        w.set(id, "Name", Value::String(name.to_string()))
            .expect("name");
        id
    }

    /// Creates a script (`Script` or `LocalScript`) with `source` under `parent`.
    pub fn add_script(
        &self,
        class: &str,
        name: &str,
        parent: InstanceId,
        source: &str,
    ) -> InstanceId {
        let id = self.add(class, name, parent);
        self.world
            .borrow_mut()
            .set(id, "Source", Value::String(source.to_string()))
            .expect("source");
        id
    }

    /// A `Script` under ServerScriptService.
    pub fn server_script(&self, name: &str, source: &str) -> InstanceId {
        self.add_script("Script", name, self.root("ServerScriptService"), source)
    }

    /// A `LocalScript` under StarterPlayer.StarterPlayerScripts.
    pub fn local_script(&self, name: &str, source: &str) -> InstanceId {
        let scripts = self
            .world
            .borrow()
            .find_first_child(self.root("StarterPlayer"), "StarterPlayerScripts")
            .expect("StarterPlayerScripts");
        self.add_script("LocalScript", name, scripts, source)
    }

    pub fn lines(&self) -> Vec<String> {
        self.out.lines.borrow().clone()
    }

    /// Steps `n` times, 1/60 s apart, starting after `from`. Returns the final time.
    pub fn run(&mut self, from: f64, n: usize) -> f64 {
        let mut now = from;
        for _ in 0..n {
            now += 1.0 / 60.0;
            self.host.step(now, 1.0 / 60.0);
        }
        now
    }
}
