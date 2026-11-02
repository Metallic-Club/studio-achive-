//! Role, ownership and player lifecycle behavior through the public host and real Luau.
mod common;

use common::Rig;
use metallic_core::{InstanceId, Value};
use metallic_script::{GuiEvent, GuiEventKind, GuiPointer, ScriptHost, ScriptRole};

fn rig(role: ScriptRole) -> Rig {
    let mut rig = Rig::new();
    rig.host = ScriptHost::with_role(rig.world.clone(), rig.out.clone(), role);
    rig
}

fn player(rig: &Rig, name: &str) -> InstanceId {
    rig.add("Player", name, rig.root("Players"))
}

fn gui(rig: &Rig, player: InstanceId) -> InstanceId {
    rig.add("PlayerGui", "PlayerGui", player)
}

fn character(rig: &Rig, player: InstanceId, name: &str) -> InstanceId {
    let model = rig.add("Model", name, rig.root("Workspace"));
    rig.world
        .borrow_mut()
        .set(player, "Character", Value::Ref(Some(model)))
        .unwrap();
    model
}

fn count(rig: &Rig, expected: &str) -> usize {
    rig.lines()
        .iter()
        .filter(|line| line.as_str() == expected)
        .count()
}

fn click(target: InstanceId) -> GuiEvent {
    GuiEvent {
        target,
        kind: GuiEventKind::MouseButton1Click,
        pointer: GuiPointer::Mouse,
        position: [12.4, 30.6],
    }
}

#[test]
fn gui_events_preserve_role_routing_order_and_pointer_payloads() {
    for role in [ScriptRole::Combined, ScriptRole::Server, ScriptRole::Client] {
        let mut rig = rig(role);
        let hud = rig.add("ScreenGui", "Hud", rig.root("Workspace"));
        let button = rig.add("TextButton", "Go", hud);
        rig.server_script("Server", "print('server-ready') workspace.Hud.Go.Activated:Connect(function() print('wrong-server-gui') end)");
        rig.local_script(
            "Ui",
            r#"
            local button = workspace.Hud.Go
            button.MouseButton1Click:Connect(function(...)
                print('click', select('#', ...), game.Players.LocalPlayer.Name)
            end)
            button.Activated:Connect(function(input, clicks)
                print('activated', input.UserInputType.Name, input.UserInputState.Name, input.Position, clicks)
            end)
            button.InputChanged:Connect(function(input)
                print('changed', input.UserInputType.Name, input.UserInputState.Name, input.Position)
            end)
            "#,
        );
        if role == ScriptRole::Client {
            let local = player(&rig, "Bound");
            rig.host.set_local_player(local).unwrap();
        }
        rig.host.start().unwrap();
        rig.host.gui_event(click(button));
        rig.host.gui_event(GuiEvent {
            target: button,
            kind: GuiEventKind::Activated,
            pointer: GuiPointer::Touch,
            position: [12.4, 30.6],
        });
        rig.host.gui_event(GuiEvent {
            target: button,
            kind: GuiEventKind::InputChanged,
            pointer: GuiPointer::Mouse,
            position: [12.4, 30.6],
        });
        rig.run(0.0, 2);
        let mut expected = Vec::new();
        if role != ScriptRole::Client {
            expected.push("print:server-ready");
        }
        if role == ScriptRole::Combined {
            expected.push("print:click 0 Player1");
        } else if role == ScriptRole::Client {
            expected.push("print:click 0 Bound");
        }
        if role != ScriptRole::Server {
            expected.extend([
                "print:activated Touch End 12, 31, 0 1",
                "print:changed MouseMovement Change 12, 31, 0",
            ]);
        }
        assert_eq!(rig.lines(), expected, "role={role:?}");
    }
}

#[test]
fn queued_gui_events_do_not_cross_client_binding_changes() {
    let mut rig = rig(ScriptRole::Client);
    let first = player(&rig, "First");
    let second = player(&rig, "Second");
    let hud = rig.add("ScreenGui", "Hud", rig.root("Workspace"));
    let button = rig.add("TextButton", "Go", hud);
    rig.local_script("Ui", "workspace.Hud.Go.MouseButton1Click:Connect(function() print('click',game.Players.LocalPlayer.Name) end)");
    rig.host.add_player(first).unwrap();
    rig.host.add_player(second).unwrap();
    rig.host.set_local_player(first).unwrap();
    rig.host.start().unwrap();
    rig.host.gui_event(click(button));
    let now = rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:click First"]);

    rig.host.gui_event(click(button));
    assert!(!rig.host.set_local_player(first).unwrap());
    let now = rig.run(now, 1);
    assert_eq!(
        count(&rig, "print:click First"),
        2,
        "a no-op binding retains valid input"
    );

    rig.host.gui_event(click(button));
    assert!(rig.host.set_local_player(second).unwrap());
    let now = rig.run(now, 1);
    assert_eq!(
        count(&rig, "print:click Second"),
        0,
        "queued input belongs to the previous binding"
    );
    rig.host.gui_event(click(button));
    let now = rig.run(now, 1);
    assert_eq!(
        count(&rig, "print:click Second"),
        1,
        "fresh GUI input must still work"
    );

    rig.host.gui_event(click(button));
    assert!(rig.host.remove_player(second).unwrap());
    rig.host.set_local_player(first).unwrap();
    let now = rig.run(now, 1);
    assert_eq!(count(&rig, "print:click First"), 2);
    rig.host.gui_event(click(button));
    rig.run(now, 1);
    assert_eq!(count(&rig, "print:click First"), 3);
    assert!(
        rig.lines()
            .iter()
            .all(|line| line.starts_with("print:click "))
    );
}

#[test]
fn client_gui_dispatch_waits_for_world_scope_and_cancels_transferred_owners() {
    let mut rig = rig(ScriptRole::Client);
    let local = player(&rig, "Local");
    let other = player(&rig, "Other");
    let owned_gui = gui(&rig, local);
    let button = rig.add("TextButton", "Go", owned_gui);
    rig.add_script(
        "LocalScript",
        "Ui",
        owned_gui,
        "script.Parent.Go.MouseButton1Click:Connect(function() print('owned-click') end)",
    );
    rig.host.set_local_player(local).unwrap();
    rig.host.start().unwrap();
    rig.host.gui_event(click(button));
    let now = rig.run(0.0, 1);
    assert_eq!(count(&rig, "print:owned-click"), 1);

    rig.host.gui_event(click(button));
    {
        let shared = rig.world.clone();
        let _borrow = shared.borrow_mut();
        rig.host.step(now + 1.0 / 60.0, 1.0 / 60.0);
    }
    assert_eq!(count(&rig, "print:owned-click"), 1);
    let now = rig.run(now, 1);
    assert_eq!(
        count(&rig, "print:owned-click"),
        2,
        "valid queued GUI work survives a borrowed-World retry"
    );

    rig.host.gui_event(click(button));
    rig.world
        .borrow_mut()
        .set_parent(owned_gui, Some(other))
        .unwrap();
    let now = rig.run(now, 1);
    assert_eq!(
        count(&rig, "print:owned-click"),
        2,
        "a transferred script cannot receive queued GUI input"
    );
    let fresh_gui = gui(&rig, local);
    let fresh = rig.add("TextButton", "Go", fresh_gui);
    rig.add_script(
        "LocalScript",
        "Fresh",
        fresh_gui,
        "script.Parent.Go.MouseButton1Click:Connect(function() print('fresh-click') end)",
    );
    let now = rig.run(now, 1);
    rig.host.gui_event(click(fresh));
    rig.run(now, 1);
    assert_eq!(count(&rig, "print:fresh-click"), 1);
    assert!(
        rig.lines()
            .iter()
            .all(|line| line == "print:owned-click" || line == "print:fresh-click")
    );
}

#[test]
fn all_roles_report_the_correct_vm_context_and_preserve_default_constructors() {
    assert_eq!(Rig::new().host.role(), ScriptRole::Combined);
    let empty = rig(ScriptRole::Server);
    assert_eq!(
        ScriptHost::new_server(empty.world.clone(), empty.out.clone()).role(),
        ScriptRole::Server
    );
    for role in [ScriptRole::Combined, ScriptRole::Server, ScriptRole::Client] {
        let mut rig = rig(role);
        rig.server_script("Server", "local r=game:GetService('RunService') print('server',r:IsServer(),r:IsClient(),game.Players.LocalPlayer)");
        rig.local_script("Client", "local r=game:GetService('RunService') print('client',r:IsServer(),r:IsClient(),game.Players.LocalPlayer.Name)");
        if role == ScriptRole::Client {
            let local = player(&rig, "Local");
            rig.host.set_local_player(local).unwrap();
        }
        rig.host.start().unwrap();
        assert!(rig.host.is_running());
        rig.run(0.0, 1);
        let expected = match role {
            ScriptRole::Combined => vec![
                "print:server true false nil",
                "print:client false true Player1",
            ],
            ScriptRole::Server => vec!["print:server true false nil"],
            ScriptRole::Client => vec!["print:client false true Local"],
        };
        assert_eq!(rig.lines(), expected, "role={role:?}");
        rig.host.stop();
        assert!(!rig.host.is_running());
    }
}

#[test]
fn client_waits_for_binding_and_never_creates_a_player_or_runs_server_scripts() {
    let mut rig = rig(ScriptRole::Client);
    rig.server_script("Trap", "error('server ran in Client')");
    rig.local_script("Starter", "print('starter',game.Players.LocalPlayer.Name)");
    rig.host.start().unwrap();
    let now = rig.run(0.0, 3);
    assert!(rig.lines().is_empty());
    assert!(rig.world.borrow().children(rig.root("Players")).is_empty());
    let local = player(&rig, "Local");
    let other = player(&rig, "Other");
    let own_gui = gui(&rig, local);
    let other_gui = gui(&rig, other);
    rig.add_script(
        "LocalScript",
        "Own",
        own_gui,
        "print('gui',game.Players.LocalPlayer.Name)",
    );
    rig.add_script(
        "LocalScript",
        "Trap",
        other_gui,
        "error('other player GUI ran')",
    );
    rig.host.add_player(local).unwrap();
    rig.host.add_player(other).unwrap();
    assert!(rig.host.set_local_player(local).unwrap());
    assert!(
        rig.lines().is_empty(),
        "binding must not run scripts outside step"
    );
    rig.run(now, 2);
    assert_eq!(rig.lines(), ["print:gui Local", "print:starter Local"]);
    assert_eq!(rig.world.borrow().children(rig.root("Players")).len(), 2);
    assert!(
        rig.world
            .borrow()
            .find_first_child(rig.root("Players"), "Player1")
            .is_none()
    );
}

#[test]
fn client_ownership_tracks_whole_gui_reparenting_and_cancels_old_work() {
    let mut rig = rig(ScriptRole::Client);
    let local = player(&rig, "Local");
    let other = player(&rig, "Other");
    let own_gui = gui(&rig, local);
    let other_gui = gui(&rig, other);
    rig.add_script("LocalScript", "Owned", own_gui, "print('start') task.delay(0.1,function() print('delayed') end) game:GetService('RunService').Heartbeat:Connect(function() print('beat') end)");
    let late = rig.add_script("LocalScript", "Late", other_gui, "print('late-owned')");
    rig.host.set_local_player(local).unwrap();
    rig.host.start().unwrap();
    let mut now = rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:start", "print:beat"]);
    rig.world
        .borrow_mut()
        .set_parent(own_gui, Some(other))
        .unwrap();
    now = rig.run(now, 20);
    assert_eq!(rig.lines(), ["print:start", "print:beat"]);
    rig.world
        .borrow_mut()
        .set_parent(own_gui, Some(local))
        .unwrap();
    rig.world
        .borrow_mut()
        .set_parent(late, Some(own_gui))
        .unwrap();
    rig.run(now, 20);
    assert_eq!(count(&rig, "print:start"), 2);
    assert_eq!(count(&rig, "print:delayed"), 1);
    assert_eq!(count(&rig, "print:late-owned"), 1);
    assert_eq!(count(&rig, "print:beat"), 21);
    assert!(rig.lines().iter().all(|line| line.starts_with("print:")));
}

#[test]
fn changing_only_character_reference_rechecks_scope_without_restarting_starter_scripts() {
    let mut rig = rig(ScriptRole::Client);
    let local = player(&rig, "Local");
    let old = character(&rig, local, "Old");
    let replacement = rig.add("Model", "Replacement", rig.root("Workspace"));
    rig.local_script("Starter", "print('starter')");
    rig.add_script(
        "LocalScript",
        "OldScript",
        old,
        "print('old') task.delay(0.1,function() error('old Character task survived') end)",
    );
    rig.add_script("LocalScript", "NewScript", replacement, "print('new')");
    rig.host.set_local_player(local).unwrap();
    rig.host.start().unwrap();
    let mut now = rig.run(0.0, 1);
    rig.world
        .borrow_mut()
        .set(local, "Character", Value::Ref(Some(replacement)))
        .unwrap();
    now = rig.run(now, 20);
    assert_eq!(count(&rig, "print:old"), 1);
    assert_eq!(count(&rig, "print:new"), 1);
    assert_eq!(count(&rig, "print:starter"), 1);
    let storage = rig.root("ReplicatedStorage");
    rig.world
        .borrow_mut()
        .set_parent(replacement, Some(storage))
        .unwrap();
    now = rig.run(now, 2);
    let workspace = rig.root("Workspace");
    rig.world
        .borrow_mut()
        .set_parent(replacement, Some(workspace))
        .unwrap();
    rig.run(now, 2);
    assert_eq!(count(&rig, "print:new"), 2);
    assert_eq!(count(&rig, "print:starter"), 1);
    assert!(
        rig.lines().iter().all(|line| line.starts_with("print:")),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn pending_add_flushes_once_before_removing_and_caller_owns_destruction() {
    let mut rig = rig(ScriptRole::Server);
    rig.server_script(
        "Roster",
        r#"
local p=game:GetService('Players')
p.PlayerAdded:Connect(function(who) print('added',who.Name,#p:GetPlayers()) end)
p.PlayerRemoving:Connect(function(who)
    assert(who.Parent==p and who.Character.Parent==workspace)
    print('removing',who.Name,#p:GetPlayers(),who.Character.Name)
end)
game:GetService('RunService').Heartbeat:Connect(function() print('count',#p:GetPlayers()) end)
"#,
    );
    rig.host.start().unwrap();
    let local = player(&rig, "Local");
    let avatar = character(&rig, local, "Avatar");
    assert!(rig.host.add_player(local).unwrap());
    assert!(!rig.host.add_player(local).unwrap());
    assert!(rig.lines().is_empty());
    assert!(rig.host.remove_player(local).unwrap());
    assert!(!rig.host.remove_player(local).unwrap());
    assert_eq!(
        rig.lines(),
        ["print:added Local 1", "print:removing Local 1 Avatar"]
    );
    assert!(rig.world.borrow().is_alive(local) && rig.world.borrow().is_alive(avatar));
    rig.world.borrow_mut().destroy(avatar);
    rig.world.borrow_mut().destroy(local);
    rig.run(0.0, 1);
    assert_eq!(
        rig.lines(),
        [
            "print:added Local 1",
            "print:removing Local 1 Avatar",
            "print:count 0"
        ]
    );
    assert!(!rig.host.remove_player(local).unwrap());
}

#[test]
fn delivered_players_remove_once_and_restart_replays_only_live_registered_players_in_world_order() {
    let mut rig = rig(ScriptRole::Server);
    let first = player(&rig, "First");
    let second = player(&rig, "Second");
    rig.server_script("Roster", "print('session') local p=game:GetService('Players') p.PlayerAdded:Connect(function(v) print('added',v.Name) end) p.PlayerRemoving:Connect(function(v) print('removed',v.Name) end)");
    rig.host.add_player(second).unwrap();
    rig.host.add_player(first).unwrap();
    rig.host.start().unwrap();
    rig.run(0.0, 1);
    assert!(rig.host.remove_player(first).unwrap());
    assert!(!rig.host.remove_player(first).unwrap());
    rig.world.borrow_mut().destroy(first);
    rig.host.stop();
    rig.world.borrow_mut().destroy(second);
    let replacement = player(&rig, "Replacement");
    assert_ne!(replacement, second);
    rig.host.add_player(replacement).unwrap();
    rig.host.start().unwrap();
    rig.run(0.0, 1);
    assert!(!rig.host.remove_player(second).unwrap());
    assert_eq!(
        rig.lines(),
        [
            "print:session",
            "print:added First",
            "print:added Second",
            "print:removed First",
            "print:session",
            "print:added Replacement"
        ]
    );
}

#[test]
fn validation_and_borrow_failure_do_not_replace_a_valid_binding_or_roster() {
    let mut rig = rig(ScriptRole::Client);
    let local = player(&rig, "Local");
    let other = player(&rig, "Other");
    let part = rig.add("Part", "WrongClass", rig.root("Workspace"));
    let orphan = rig.world.borrow_mut().create("Player", None).unwrap();
    let stale = player(&rig, "Gone");
    rig.world.borrow_mut().destroy(stale);
    rig.local_script("Observe", "print(game.Players.LocalPlayer.Name) game:GetService('RunService').Heartbeat:Connect(function() print(#game.Players:GetPlayers()) end)");
    rig.host.set_local_player(local).unwrap();
    rig.host.add_player(local).unwrap();
    for invalid in [part, orphan, stale] {
        assert!(rig.host.set_local_player(invalid).is_err());
        assert!(rig.host.add_player(invalid).is_err());
        assert!(!rig.host.remove_player(invalid).unwrap());
    }
    {
        let shared = rig.world.clone();
        let _borrow = shared.borrow_mut();
        assert!(rig.host.add_player(other).is_err());
        assert!(rig.host.set_local_player(other).is_err());
        assert!(rig.host.remove_player(local).is_err());
    }
    rig.host.start().unwrap();
    rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:Local", "print:1"]);
    let mut server = ScriptHost::new_server(rig.world.clone(), rig.out.clone());
    assert!(server.set_local_player(local).is_err());
    let mut combined = ScriptHost::new(rig.world.clone(), rig.out.clone());
    assert!(combined.set_local_player(local).is_err());
}

#[test]
fn rebinding_and_removing_client_cancel_old_tasks_connections_and_allow_unbound_running_state() {
    let mut rig = rig(ScriptRole::Client);
    let first = player(&rig, "First");
    let second = player(&rig, "Second");
    rig.local_script("Lifecycle", "local name=game.Players.LocalPlayer.Name print('start',name) task.delay(0.1,function() print('later',name) end) game:GetService('RunService').Heartbeat:Connect(function() print('beat',name) end)");
    rig.host.add_player(first).unwrap();
    rig.host.add_player(second).unwrap();
    rig.host.set_local_player(first).unwrap();
    rig.host.start().unwrap();
    let mut now = rig.run(0.0, 1);
    assert!(rig.host.set_local_player(second).unwrap());
    assert!(!rig.host.set_local_player(second).unwrap());
    now = rig.run(now, 20);
    assert_eq!(count(&rig, "print:start First"), 1);
    assert_eq!(count(&rig, "print:beat First"), 1);
    assert_eq!(count(&rig, "print:later First"), 0);
    assert_eq!(count(&rig, "print:start Second"), 1);
    assert_eq!(count(&rig, "print:later Second"), 1);
    assert!(rig.host.remove_player(second).unwrap());
    assert!(
        rig.host.is_running(),
        "Client VM stays running without a local binding"
    );
    let after_removal = rig.lines();
    rig.host.dispatch_input_now(now);
    rig.host.end_frame();
    assert_eq!(
        rig.lines(),
        after_removal,
        "removed local work must stop before the caller destroys its still-live Player"
    );
    rig.world.borrow_mut().destroy(second);
    let before = rig.lines();
    now = rig.run(now, 20);
    assert_eq!(rig.lines(), before);
    rig.host.set_local_player(first).unwrap();
    rig.run(now, 1);
    assert_eq!(count(&rig, "print:start First"), 2);
    assert!(
        rig.lines().iter().all(|line| line.starts_with("print:")),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn stale_binding_across_restart_does_not_bind_a_recreated_same_name_player() {
    let mut rig = rig(ScriptRole::Client);
    let old = player(&rig, "Local");
    rig.local_script("Observe", "print('bound',game.Players.LocalPlayer.Name)");
    rig.host.add_player(old).unwrap();
    rig.host.set_local_player(old).unwrap();
    rig.host.start().unwrap();
    rig.run(0.0, 1);
    rig.host.stop();
    rig.world.borrow_mut().destroy(old);
    let replacement = player(&rig, "Local");
    assert_ne!(old, replacement);
    rig.host.start().unwrap();
    let now = rig.run(0.0, 2);
    assert_eq!(rig.lines(), ["print:bound Local"]);
    assert!(rig.host.set_local_player(old).is_err());
    assert!(!rig.host.remove_player(old).unwrap());
    rig.host.add_player(replacement).unwrap();
    rig.host.set_local_player(replacement).unwrap();
    rig.run(now, 1);
    assert_eq!(rig.lines(), ["print:bound Local", "print:bound Local"]);
    assert_eq!(rig.world.borrow().children(rig.root("Players")).len(), 1);
}

#[test]
fn invalidated_active_binding_does_not_revive_after_player_reparenting() {
    let mut rig = rig(ScriptRole::Client);
    let local = player(&rig, "Local");
    rig.local_script(
        "Observe",
        "print('bound') task.delay(0.1,function() print('delayed') end)",
    );
    rig.host.add_player(local).unwrap();
    rig.host.set_local_player(local).unwrap();
    rig.host.start().unwrap();
    let mut now = rig.run(0.0, 1);
    rig.world.borrow_mut().set_parent(local, None).unwrap();
    now = rig.run(now, 1);
    let players = rig.root("Players");
    rig.world
        .borrow_mut()
        .set_parent(local, Some(players))
        .unwrap();
    now = rig.run(now, 20);
    assert_eq!(rig.lines(), ["print:bound"]);
    assert!(
        rig.host.add_player(local).unwrap(),
        "invalid scope forgets old enrollment"
    );
    rig.host.set_local_player(local).unwrap();
    rig.run(now, 1);
    assert_eq!(rig.lines(), ["print:bound", "print:bound"]);
}

#[test]
fn removal_and_caller_destruction_cancel_character_script_work_without_stopping_global_scripts() {
    let mut rig = rig(ScriptRole::Server);
    let local = player(&rig, "Local");
    let avatar = character(&rig, local, "Avatar");
    rig.add_script("Script", "Owned", avatar, "print('character') task.delay(0.1,function() error('destroyed Character task survived') end) game:GetService('RunService').Heartbeat:Connect(function() print('character-beat') end)");
    rig.server_script(
        "Global",
        "task.delay(0.1,function() print('global-alive') end)",
    );
    rig.host.add_player(local).unwrap();
    rig.host.start().unwrap();
    let now = rig.run(0.0, 1);
    rig.host.remove_player(local).unwrap();
    rig.world.borrow_mut().destroy(avatar);
    rig.world.borrow_mut().destroy(local);
    rig.run(now, 20);
    assert_eq!(count(&rig, "print:character"), 1);
    assert_eq!(count(&rig, "print:character-beat"), 1);
    assert_eq!(count(&rig, "print:global-alive"), 1);
    assert!(
        rig.lines().iter().all(|line| line.starts_with("print:")),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn stopped_registration_cancellation_emits_no_lifecycle_and_cannot_bind_client() {
    let mut rig = rig(ScriptRole::Client);
    let local = player(&rig, "Local");
    rig.local_script("Observe", "print('client') game.Players.PlayerAdded:Connect(function() print('added') end) game.Players.PlayerRemoving:Connect(function() print('removing') end)");
    rig.host.add_player(local).unwrap();
    rig.host.set_local_player(local).unwrap();
    assert!(rig.host.remove_player(local).unwrap());
    rig.host.start().unwrap();
    let now = rig.run(0.0, 1);
    assert!(rig.lines().is_empty());
    rig.host.add_player(local).unwrap();
    rig.host.set_local_player(local).unwrap();
    rig.run(now, 1);
    assert_eq!(rig.lines(), ["print:client", "print:added"]);
}

const SLOW_REMOVING_HANDLER: &str = r#"
game.Players.PlayerRemoving:Connect(function()
    local untilTime=os.clock()+0.04
    while os.clock()<untilTime do end
    print('removed-handler')
end)
"#;

#[test]
fn synchronous_removal_uses_one_aggregate_budget_for_independent_owners() {
    let mut rig = rig(ScriptRole::Server);
    rig.host
        .set_step_budget_sec(metallic_script::STEP_BUDGET_SEC);
    for index in 0..8 {
        rig.server_script(&format!("Owner{index}"), SLOW_REMOVING_HANDLER);
    }
    let local = player(&rig, "Local");
    rig.host.add_player(local).unwrap();
    rig.host.start().unwrap();
    let now = rig.run(0.0, 1);
    assert!(rig.host.remove_player(local).unwrap());
    let synchronous = count(&rig, "print:removed-handler");
    assert!(
        synchronous < 8,
        "all {synchronous} independent handlers ran outside aggregate pacing"
    );
    // Deferred handlers may run after destruction and therefore must not assume their
    // instance argument is still live. This fixture deliberately ignores that argument.
    rig.world.borrow_mut().destroy(local);
    rig.run(now, 20);
    assert_eq!(count(&rig, "print:removed-handler"), 8);
    assert!(
        rig.lines()
            .iter()
            .all(|line| line == "print:removed-handler"),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn removal_preserves_a_held_input_budget_instead_of_resetting_it() {
    let mut rig = rig(ScriptRole::Combined);
    rig.host
        .set_step_budget_sec(metallic_script::STEP_BUDGET_SEC);
    for index in 0..8 {
        rig.local_script(&format!("Owner{index}"), SLOW_REMOVING_HANDLER);
    }
    rig.host.start().unwrap();
    let other = player(&rig, "Other");
    rig.host.add_player(other).unwrap();
    let now = rig.run(0.0, 1);
    rig.host.dispatch_input_now(now);
    // A held frame budget has elapsed; remove must preserve it instead of granting a
    // new 250 ms allowance. There is no upper wall-time performance assertion here.
    std::thread::sleep(std::time::Duration::from_millis(275));
    rig.host.remove_player(other).unwrap();
    assert_eq!(count(&rig, "print:removed-handler"), 0);
    rig.world.borrow_mut().destroy(other);
    rig.host.end_frame();
    rig.run(now, 20);
    assert_eq!(count(&rig, "print:removed-handler"), 8);
    assert!(
        rig.lines()
            .iter()
            .all(|line| line == "print:removed-handler"),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn input_dispatch_does_not_resume_deferred_work_after_gui_ownership_changes() {
    let mut rig = rig(ScriptRole::Client);
    rig.host
        .set_step_budget_sec(metallic_script::STEP_BUDGET_SEC);
    let local = player(&rig, "Local");
    let other = player(&rig, "Other");
    let leaving = player(&rig, "Leaving");
    let owned_gui = gui(&rig, local);
    rig.local_script("Global", "print('global-start') game:GetService('RunService').Heartbeat:Connect(function() print('global-beat') end)");
    for index in 0..8 {
        rig.add_script(
            "LocalScript",
            &format!("Owner{index}"),
            owned_gui,
            &format!("print('connected') {SLOW_REMOVING_HANDLER}"),
        );
    }
    for p in [local, other, leaving] {
        rig.host.add_player(p).unwrap();
    }
    rig.host.set_local_player(local).unwrap();
    rig.host.start().unwrap();
    let now = rig.run(0.0, 1);
    assert_eq!(count(&rig, "print:connected"), 8);
    rig.host.remove_player(leaving).unwrap();
    let before = count(&rig, "print:removed-handler");
    assert!(before < 8, "fixture must retain deferred work");
    rig.world.borrow_mut().destroy(leaving);
    rig.world
        .borrow_mut()
        .set_parent(owned_gui, Some(other))
        .unwrap();
    let fresh_gui = gui(&rig, local);
    rig.add_script("LocalScript", "Late", fresh_gui, "print('late-start')");
    rig.host.dispatch_input_now(now);
    rig.host.end_frame();
    assert_eq!(
        count(&rig, "print:removed-handler"),
        before,
        "explicit input dispatch resumed a script now owned by another player"
    );
    assert_eq!(
        count(&rig, "print:late-start"),
        0,
        "pruning must not start new scripts"
    );
    rig.run(now, 2);
    assert_eq!(count(&rig, "print:removed-handler"), before);
    assert_eq!(
        count(&rig, "print:global-start"),
        1,
        "unrelated starter work must not restart"
    );
    assert_eq!(
        count(&rig, "print:global-beat"),
        3,
        "unrelated valid handlers must continue"
    );
    assert_eq!(
        count(&rig, "print:late-start"),
        1,
        "pruning must not consume pending start changes"
    );
}

#[test]
fn lifecycle_dispatch_does_not_call_handlers_after_gui_ownership_changes() {
    let mut rig = rig(ScriptRole::Client);
    let local = player(&rig, "Local");
    let other = player(&rig, "Other");
    let first = player(&rig, "FirstLeaving");
    let second = player(&rig, "SecondLeaving");
    let owned_gui = gui(&rig, local);
    rig.add_script(
        "LocalScript",
        "Observe",
        owned_gui,
        "print('connected') game.Players.PlayerRemoving:Connect(function() print('removed') end)",
    );
    for p in [local, other, first, second] {
        rig.host.add_player(p).unwrap();
    }
    rig.host.set_local_player(local).unwrap();
    rig.host.start().unwrap();
    let now = rig.run(0.0, 1);
    rig.host.remove_player(first).unwrap();
    rig.world.borrow_mut().destroy(first);
    assert_eq!(rig.lines(), ["print:connected", "print:removed"]);
    rig.world
        .borrow_mut()
        .set_parent(owned_gui, Some(other))
        .unwrap();
    rig.host.remove_player(second).unwrap();
    rig.world.borrow_mut().destroy(second);
    assert_eq!(
        rig.lines(),
        ["print:connected", "print:removed"],
        "lifecycle dispatch called a script now owned by another player"
    );
    rig.run(now, 2);
    assert_eq!(rig.lines(), ["print:connected", "print:removed"]);
}

#[test]
fn client_input_dispatch_postpones_lua_when_world_scope_is_borrowed() {
    let mut rig = rig(ScriptRole::Client);
    rig.host
        .set_step_budget_sec(metallic_script::STEP_BUDGET_SEC);
    let local = player(&rig, "Local");
    let leaving = player(&rig, "Leaving");
    let owned_gui = gui(&rig, local);
    for index in 0..8 {
        rig.add_script(
            "LocalScript",
            &format!("Owner{index}"),
            owned_gui,
            SLOW_REMOVING_HANDLER,
        );
    }
    rig.host.add_player(local).unwrap();
    rig.host.add_player(leaving).unwrap();
    rig.host.set_local_player(local).unwrap();
    rig.host.start().unwrap();
    let now = rig.run(0.0, 1);
    rig.host.remove_player(leaving).unwrap();
    rig.world.borrow_mut().destroy(leaving);
    let before = count(&rig, "print:removed-handler");
    assert!(before < 8, "fixture must retain deferred work");
    {
        let shared = rig.world.clone();
        let _borrow = shared.borrow_mut();
        rig.host.dispatch_input_now(now);
    }
    rig.host.end_frame();
    assert_eq!(
        count(&rig, "print:removed-handler"),
        before,
        "Client dispatch executed Lua without validating its borrowed World scope"
    );
    rig.run(now, 20);
    assert_eq!(
        count(&rig, "print:removed-handler"),
        8,
        "postponed valid work must remain queued"
    );
}

#[test]
fn client_step_postpones_tasks_until_reparented_owner_scope_can_be_validated() {
    let mut rig = rig(ScriptRole::Client);
    let local = player(&rig, "Local");
    let other = player(&rig, "Other");
    let owned_gui = gui(&rig, local);
    rig.add_script(
        "LocalScript",
        "Owned",
        owned_gui,
        "print('connected') task.delay(0.05,function() print('owned-task') end)",
    );
    rig.local_script(
        "Global",
        "task.delay(0.05,function() print('global-task') end)",
    );
    rig.host.set_local_player(local).unwrap();
    rig.host.start().unwrap();
    rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:connected"]);
    {
        let shared = rig.world.clone();
        let mut world = shared.borrow_mut();
        world.set_parent(owned_gui, Some(other)).unwrap();
        // Advance virtual time past both queued tasks while the changed ownership
        // remains unreadable to the host. No wall-clock sleep is involved.
        rig.host.step(0.1, 0.1);
    }
    assert_eq!(
        rig.lines(),
        ["print:connected"],
        "Client step resumed Lua before validating its borrowed World scope"
    );
    rig.run(0.1, 3);
    assert_eq!(
        rig.lines(),
        ["print:connected", "print:global-task"],
        "transferred work must cancel while unrelated valid work remains queued"
    );
}
