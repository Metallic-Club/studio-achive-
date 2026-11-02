//! Server-only host behavior through real Scripts, events and journal updates.
mod common;

use common::Rig;
use metallic_core::Value;
use metallic_script::ScriptHost;

fn server_rig() -> Rig {
    let mut rig = Rig::new();
    rig.host = ScriptHost::new_server(rig.world.clone(), rig.out.clone());
    rig
}

#[test]
fn server_startup_heartbeat_wait_and_instance_api_need_no_local_player() {
    let mut rig = server_rig();
    let marker = rig.add("Part", "Marker", rig.root("Workspace"));
    rig.add("InputAction", "Action", rig.root("ReplicatedStorage"));
    rig.add_script(
        "Script",
        "WorkspaceScript",
        rig.root("Workspace"),
        "print('workspace-start')",
    );
    rig.local_script("Trap", "error('unexpected-client-start')");
    rig.server_script(
        "Server",
        r#"
local players=game:GetService('Players')
local run=game:GetService('RunService')
assert(run:IsServer() and not run:IsClient())
assert(players.LocalPlayer==nil and #players:GetPlayers()==0)
assert(#players:GetChildren()==0,'synthetic player appeared')
assert(io==nil and loadstring==nil and type(require)=='function','sandbox changed')
assert(not pcall(function() return game:GetService('ReplicatedStorage').Action:GetState() end), 'client action state became available on the server')
players.PlayerAdded:Connect(function() error('synthetic PlayerAdded') end)
workspace.Marker.Anchored=true
print('server-start')
local count=0
local connection
connection=run.Heartbeat:Connect(function(dt)
    assert(dt==0.125)
    count+=1
    if count==2 then connection:Disconnect() print('heartbeat') end
end)
assert(task.wait(0.25)==0.25)
print('waited')
"#,
    );
    rig.host.start().unwrap();
    rig.host.start().unwrap();
    assert_eq!(rig.lines(), ["print:workspace-start", "print:server-start"]);
    assert_eq!(
        rig.world.borrow().get(marker, "Anchored").unwrap(),
        Value::Bool(true)
    );
    rig.host.step(0.125, 0.125);
    rig.host.step(0.25, 0.125);
    assert_eq!(
        rig.lines(),
        [
            "print:workspace-start",
            "print:server-start",
            "print:heartbeat",
            "print:waited"
        ]
    );
    assert!(rig.world.borrow().children(rig.root("Players")).is_empty());
}

#[test]
fn existing_world_state_is_preserved_and_late_client_scripts_never_start() {
    let mut rig = server_rig();
    let player = rig.add("Player", "Player1", rig.root("Players"));
    rig.world
        .borrow_mut()
        .set(player, "UserId", Value::Number(42.0))
        .unwrap();
    let gui = rig.add("PlayerGui", "ExistingGui", player);
    let existing = rig.add_script(
        "LocalScript",
        "Existing",
        gui,
        "error('existing client ran')",
    );
    let starter = rig
        .world
        .borrow()
        .find_first_child(rig.root("StarterPlayer"), "StarterPlayerScripts")
        .unwrap();
    let parked = rig.add_script(
        "LocalScript",
        "Parked",
        rig.root("ReplicatedStorage"),
        "error('reparented client ran')",
    );
    let disabled = rig.add_script(
        "LocalScript",
        "Disabled",
        starter,
        "error('enabled client ran')",
    );
    rig.world
        .borrow_mut()
        .set(disabled, "Enabled", Value::Bool(false))
        .unwrap();
    let before = rig.world.borrow().change_cursor();
    rig.host.start().unwrap();
    rig.host.step(0.1, 0.1);
    assert_eq!(
        rig.world.borrow().change_cursor(),
        before,
        "server startup must not rewrite existing world state"
    );
    assert!(rig.lines().is_empty(), "{:?}", rig.lines());

    let late = rig.add_script("LocalScript", "Late", starter, "error('late client ran')");
    rig.world
        .borrow_mut()
        .set(disabled, "Enabled", Value::Bool(true))
        .unwrap();
    rig.world
        .borrow_mut()
        .set_parent(parked, Some(gui))
        .unwrap();
    rig.host.step(0.2, 0.1);
    rig.world.borrow_mut().set_parent(late, Some(gui)).unwrap();
    rig.world.borrow_mut().destroy(existing);
    rig.host.step(0.3, 0.1);
    assert!(rig.lines().is_empty(), "{:?}", rig.lines());
    let world = rig.world.borrow();
    assert_eq!(world.children(rig.root("Players")), &[player]);
    assert_eq!(world.children(player), &[gui]);
    assert_eq!(world.get(player, "UserId").unwrap(), Value::Number(42.0));
    assert_eq!(world.parent(parked), Some(gui));
}

#[test]
fn server_script_journal_lifecycle_cancels_only_the_stopped_owner() {
    for stop in ["disable", "reparent", "destroy"] {
        let mut rig = server_rig();
        let pad = rig.add("Part", "Pad", rig.root("Workspace"));
        let hit = rig.add("Part", "Hit", rig.root("Workspace"));
        let script = rig.server_script(
            "Owner",
            r#"
print('active')
workspace.Pad.Touched:Connect(function() print('touch') end)
task.delay(1,function() print('escaped') end)
"#,
        );
        rig.server_script("Control", "task.delay(1,function() print('control') end)");
        rig.host.start().unwrap();
        rig.host.touched(pad, hit);
        rig.host.step(0.1, 0.1);
        assert_eq!(rig.lines(), ["print:active", "print:touch"]);
        {
            let mut world = rig.world.borrow_mut();
            let outside = world.root("ReplicatedStorage");
            match stop {
                "disable" => world.set(script, "Enabled", Value::Bool(false)).unwrap(),
                "reparent" => world.set_parent(script, outside).unwrap(),
                "destroy" => world.destroy(script),
                _ => unreachable!(),
            }
        }
        rig.host.step(0.2, 0.1);
        rig.host.touched(pad, hit);
        rig.host.step(1.1, 0.9);
        assert_eq!(
            rig.lines(),
            ["print:active", "print:touch", "print:control"],
            "{stop}"
        );
        // A newly eligible Script still starts after the role filter is applied.
        let late = rig.add_script(
            "Script",
            "Late",
            rig.root("ReplicatedStorage"),
            "print('late-server')",
        );
        rig.host.step(1.2, 0.1);
        let server_scripts = rig.root("ServerScriptService");
        rig.world
            .borrow_mut()
            .set_parent(late, Some(server_scripts))
            .unwrap();
        rig.host.step(1.3, 0.1);
        assert_eq!(
            rig.lines().last().map(String::as_str),
            Some("print:late-server")
        );
    }
}

#[test]
fn stop_restart_discards_old_tasks_events_and_connections() {
    let mut rig = server_rig();
    let pad = rig.add("Part", "Pad", rig.root("Workspace"));
    let hit = rig.add("Part", "Hit", rig.root("Workspace"));
    rig.server_script(
        "Lifecycle",
        r#"
print('start')
workspace.Pad.Touched:Connect(function() print('touch') end)
task.delay(0.5,function() print('delayed') end)
"#,
    );
    rig.host.start().unwrap();
    rig.host.touched(pad, hit);
    rig.host.step(0.1, 0.1);
    rig.host.touched(pad, hit);
    rig.host.stop();
    rig.host.stop();
    assert!(!rig.host.is_running());
    rig.host.step(1.0, 0.9);
    assert_eq!(rig.lines(), ["print:start", "print:touch"]);
    rig.host.start().unwrap();
    rig.host.step(0.1, 0.1);
    assert_eq!(rig.lines(), ["print:start", "print:touch", "print:start"]);
    rig.host.step(0.6, 0.5);
    rig.host.touched(pad, hit);
    rig.host.step(0.7, 0.1);
    assert_eq!(
        rig.lines(),
        [
            "print:start",
            "print:touch",
            "print:start",
            "print:delayed",
            "print:touch"
        ]
    );
    assert!(rig.world.borrow().children(rig.root("Players")).is_empty());
}

#[test]
fn client_targeted_remotes_have_no_receiver_and_do_not_accumulate_across_steps() {
    let mut rig = server_rig();
    rig.add("RemoteEvent", "Notify", rig.root("ReplicatedStorage"));
    rig.local_script("Trap", "error('client receiver was created')");
    rig.server_script(
        "Remotes",
        r#"
local remote=game:GetService('ReplicatedStorage').Notify
for i=1,600 do remote:FireAllClients('startup') end
local count=0
game:GetService('RunService').Heartbeat:Connect(function()
    for i=1,600 do remote:FireAllClients('no-receiver') end
    count+=1
    if count==5 then print('five-steps') end
end)
"#,
    );
    rig.host.start().unwrap();
    rig.run(0.0, 5);
    assert_eq!(rig.lines(), ["print:five-steps"]);
    assert!(rig.world.borrow().children(rig.root("Players")).is_empty());
    rig.host.stop();
}

#[test]
fn default_constructor_still_runs_both_sides_and_delivers_local_player_added() {
    let mut rig = Rig::new();
    rig.server_script(
        "Server",
        r#"
print('server')
game:GetService('Players').PlayerAdded:Connect(function(player) print('joined:'..player.Name) end)
"#,
    );
    rig.local_script(
        "Client",
        r#"
assert(game:GetService('RunService'):IsClient())
assert(game:GetService('Players').LocalPlayer.Name=='Player1')
print('client')
"#,
    );
    rig.host.start().unwrap();
    assert_eq!(rig.lines(), ["print:server", "print:client"]);
    rig.host.step(0.1, 0.1);
    assert_eq!(
        rig.lines(),
        ["print:server", "print:client", "print:joined:Player1"]
    );
}
