//! [`Debugger`]: the host-level debugger API over the server and client VMs.

use metallic_core::InstanceId;

use super::{
    Breakpoint, DebugState, Debugger, Frame, PauseReason, Scope, StepKind, VarHandle, VarNode,
};
use crate::ScriptHost;
use crate::host::{Side, owner_id};
use crate::vm::ScriptError;

fn not_paused() -> ScriptError {
    ScriptError {
        message: "the game is not paused".into(),
    }
}

impl ScriptHost {
    /// Sends the breakpoints of `only` (or of every script) and break-on-error to `side`.
    pub(crate) fn push_breakpoints(&self, side: &Side, only: Option<InstanceId>) {
        let debug = &side.vm.sched().debug;
        let lua = side.vm.lua();
        let ids: Vec<InstanceId> = match only {
            Some(id) => vec![id],
            None => self.breakpoints.keys().copied().collect(),
        };
        for id in ids {
            let list: Vec<(u32, Breakpoint)> = self
                .breakpoints
                .get(&id)
                .map(|lines| lines.iter().map(|(l, b)| (*l, b.clone())).collect())
                .unwrap_or_default();
            for warning in debug.breakpoints.set_owner(lua, owner_id(id), &list) {
                self.output.warn(&warning);
            }
        }
        debug.set_break_on_error(self.break_on_error);
    }

    fn paused_side(&self) -> Option<&Side> {
        self.sides()
            .into_iter()
            .find(|side| side.vm.sched().debug.is_paused())
    }

    /// The script (or ModuleScript) an owner id stands for, if it is still in the world.
    fn script_of(&self, owner: u64) -> Option<InstanceId> {
        if let Some(id) = self
            .running
            .keys()
            .copied()
            .find(|&id| owner_id(id) == owner)
        {
            return Some(id);
        }
        let id = InstanceId::from_bits(owner.checked_sub(1)?);
        let world = self.world.try_borrow().ok()?;
        world.is_alive(id).then_some(id)
    }
}

impl Debugger<'_> {
    /// Sets (or replaces) the breakpoint on `line` of `script`. It applies now if the
    /// script is running and to every later start.
    pub fn set_breakpoint(&mut self, script: InstanceId, line: u32, kind: Breakpoint) {
        self.host
            .breakpoints
            .entry(script)
            .or_default()
            .insert(line, kind);
        self.sync(script);
    }

    /// Removes the breakpoint on `line` of `script`, if any.
    pub fn clear_breakpoint(&mut self, script: InstanceId, line: u32) {
        if let Some(lines) = self.host.breakpoints.get_mut(&script) {
            lines.remove(&line);
            if lines.is_empty() {
                self.host.breakpoints.remove(&script);
            }
        }
        self.sync(script);
    }

    fn sync(&mut self, script: InstanceId) {
        let host = &*self.host;
        for side in host.sides() {
            host.push_breakpoints(side, Some(script));
        }
    }

    /// Every breakpoint: script, line, kind.
    pub fn breakpoints(&self) -> Vec<(InstanceId, u32, Breakpoint)> {
        self.host
            .breakpoints
            .iter()
            .flat_map(|(id, lines)| lines.iter().map(|(l, b)| (*id, *l, b.clone())))
            .collect()
    }

    /// The line the breakpoint of `script` at `line` really stops on (the next line with
    /// code), once the script runs.
    pub fn resolved_line(&self, script: InstanceId, line: u32) -> Option<u32> {
        self.host.sides().into_iter().find_map(|side| {
            side.vm
                .sched()
                .debug
                .breakpoints
                .actual_line(owner_id(script), line)
        })
    }

    /// Pauses at errors that no `pcall` handles (off by default).
    pub fn break_on_error(&mut self, on: bool) {
        self.host.break_on_error = on;
        for side in self.host.sides() {
            side.vm.sched().debug.set_break_on_error(on);
        }
    }

    /// Whether the debugger pauses at unhandled errors.
    pub fn breaks_on_error(&self) -> bool {
        self.host.break_on_error
    }

    /// Pauses the game now (scripts never run while the host is called, so nothing is
    /// interrupted). A step from here stops in the next script code that runs.
    pub fn pause(&mut self) {
        if self.host.is_running() && !self.host.is_paused() {
            self.host.manual_pause = true;
        }
    }

    /// Continues a paused game.
    pub fn resume(&mut self) {
        self.go(None);
    }

    /// Runs to the next line of the paused frame, over calls.
    pub fn step_over(&mut self) {
        self.go(Some(StepKind::Over));
    }

    /// Runs to the next line, into called functions and spawned threads.
    pub fn step_into(&mut self) {
        self.go(Some(StepKind::Into));
    }

    /// Runs until the paused function returns to its caller.
    pub fn step_out(&mut self) {
        self.go(Some(StepKind::Out));
    }

    fn go(&mut self, step: Option<StepKind>) {
        let host = &mut *self.host;
        let manual = std::mem::replace(&mut host.manual_pause, false);
        for side in host.sides() {
            side.vm.sched().debug.set_break_next(false);
        }
        if let Some(side) = host.paused_side() {
            let sched = side.vm.sched();
            sched.debug.resume(sched, step);
        } else if manual && step.is_some() {
            for side in host.sides() {
                side.vm.sched().debug.set_break_next(true);
            }
        }
    }

    /// Single-step hook calls so far, both VMs (for tests of what runs single-stepped).
    #[doc(hidden)]
    pub fn step_hook_calls(&self) -> u64 {
        self.host
            .sides()
            .into_iter()
            .map(|side| side.vm.sched().debug.step_hooks.get())
            .sum()
    }

    /// Running, or paused with the reason and call stack.
    pub fn state(&self) -> DebugState {
        let host = &*self.host;
        if let Some(side) = host.paused_side()
            && let Some((reason, frames)) = side.vm.sched().debug.pause_info()
        {
            let stack = frames
                .into_iter()
                .map(|f| Frame {
                    script: f.owner.and_then(|o| host.script_of(o)),
                    chunk: f.chunk,
                    function: f.function,
                    line: f.line,
                })
                .collect();
            return DebugState::Paused { reason, stack };
        }
        if host.manual_pause {
            return DebugState::Paused {
                reason: PauseReason::Pause,
                stack: Vec::new(),
            };
        }
        DebugState::Running
    }

    /// The variables of `scope` in paused frame `frame` (an index into the stack of
    /// [`Debugger::state`]). Empty while running.
    pub fn variables(&self, frame: usize, scope: Scope) -> Vec<VarNode> {
        match self.host.paused_side() {
            Some(side) => {
                let sched = side.vm.sched();
                sched.debug.variables(sched, frame, scope)
            }
            None => Vec::new(),
        }
    }

    /// The children of a table or Instance row.
    pub fn expand(&self, handle: VarHandle) -> Vec<VarNode> {
        match self.host.paused_side() {
            Some(side) => {
                let sched = side.vm.sched();
                sched.debug.expand(sched, handle)
            }
            None => Vec::new(),
        }
    }

    /// Evaluates the Luau expression `expr` in paused frame `frame` (sandboxed, under the
    /// execution budget, breakpoints off) and returns its value as text.
    ///
    /// # Errors
    /// The expression's compile or runtime error, or "not paused".
    pub fn evaluate(&self, frame: usize, expr: &str) -> Result<String, ScriptError> {
        let side = self.host.paused_side().ok_or_else(not_paused)?;
        let sched = side.vm.sched();
        sched.debug.evaluate(sched, frame, expr)
    }

    /// Sets local `name` of paused frame `frame` to the value of the Luau expression
    /// `expr` (evaluated like [`Debugger::evaluate`]); the script sees it when it resumes.
    ///
    /// # Errors
    /// When the game is not paused, `name` is not a local of the frame or `expr` fails.
    pub fn set_local(&mut self, frame: usize, name: &str, expr: &str) -> Result<(), ScriptError> {
        let side = self.host.paused_side().ok_or_else(not_paused)?;
        let sched = side.vm.sched();
        sched.debug.set_local(sched, frame, name, expr)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use metallic_core::{Value, World};
    use mlua::ffi;

    use crate::debug::Breakpoint;
    use crate::{OutputSink, ScriptHost};

    struct Quiet;
    impl OutputSink for Quiet {
        fn print(&self, _: &str) {}
        fn warn(&self, _: &str) {}
        fn error(&self, _: &str) {}
    }

    /// Whether the Luau debug callbacks of the server VM are installed.
    fn hooked(host: &ScriptHost) -> bool {
        let side = &host.sides()[0];
        let mut hooked = false;
        // SAFETY: reads the callback table of a live state.
        unsafe {
            side.vm
                .lua()
                .exec_raw::<()>((), |st| {
                    let cb = ffi::lua_callbacks(st);
                    hooked = (*cb).debugstep.is_some()
                        || (*cb).debugbreak.is_some()
                        || (*cb).preresume.is_some()
                        || (*cb).debugprotectederror.is_some();
                })
                .unwrap();
        }
        assert_eq!(hooked, side.vm.sched().debug.armed());
        hooked
    }

    #[test]
    fn hooks_are_installed_only_while_needed() {
        let world = Rc::new(RefCell::new(World::new_with_services()));
        let sss = world.borrow().root("ServerScriptService").unwrap();
        let script = world.borrow_mut().create("Script", Some(sss)).unwrap();
        world
            .borrow_mut()
            .set(
                script,
                "Source",
                Value::String("local n = 0\nn += 1\n".into()),
            )
            .unwrap();
        let mut host = ScriptHost::new(world, Rc::new(Quiet));
        host.start().unwrap();
        assert!(!hooked(&host), "no breakpoints: the VM runs unhooked");
        host.debugger().set_breakpoint(script, 2, Breakpoint::Plain);
        assert!(hooked(&host));
        host.debugger().clear_breakpoint(script, 2);
        assert!(!hooked(&host));
        host.debugger().break_on_error(true);
        assert!(hooked(&host));
        host.debugger().break_on_error(false);
        assert!(!hooked(&host));
        host.debugger().pause();
        host.debugger().step_into();
        assert!(hooked(&host), "a pending step needs the hooks");
        host.debugger().resume();
        assert!(!hooked(&host));
    }
}
