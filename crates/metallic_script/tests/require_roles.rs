//! Module loading through explicit host roles and Client ownership transitions.

mod common;

use common::Rig;
use metallic_core::{InstanceId, Value};
use metallic_script::{ScriptHost, ScriptRole};

fn rig(role: ScriptRole) -> Rig {
    let mut rig = Rig::new();
    rig.host = ScriptHost::with_role(rig.world.clone(), rig.out.clone(), role);
    rig
}

fn player(rig: &Rig, name: &str) -> InstanceId {
    rig.add("Player", name, rig.root("Players"))
}

fn module(rig: &Rig, name: &str, source: &str) -> InstanceId {
    rig.add_script("ModuleScript", name, rig.root("ReplicatedStorage"), source)
}

fn count(rig: &Rig, line: &str) -> usize {
    rig.lines()
        .iter()
        .filter(|value| value.as_str() == line)
        .count()
}

#[test]
fn explicit_roles_preserve_module_visibility_and_per_vm_caches() {
    for role in [ScriptRole::Combined, ScriptRole::Server, ScriptRole::Client] {
        let mut rig = rig(role);
        module(&rig, "Shared", "print('shared loaded') return {} ");
        rig.add_script(
            "ModuleScript",
            "Secret",
            rig.root("ServerScriptService"),
            "return 'secret'",
        );
        rig.server_script(
            "Server",
            r#"
            local m = require(game.ReplicatedStorage.Shared)
            print('server cached', m == require(game.ReplicatedStorage.Shared))
            print('server secret', require(game.ServerScriptService.Secret))
        "#,
        );
        rig.local_script("Client", r#"
            local m = require(game.ReplicatedStorage.Shared)
            print('client cached', m == require(game.ReplicatedStorage.Shared))
            local ok, err = pcall(require, game.ServerScriptService.Secret)
            print('client blocked', not ok and string.find(err, 'only available on the server', 1, true) ~= nil)
        "#);
        if role == ScriptRole::Client {
            let local = player(&rig, "Local");
            rig.host.set_local_player(local).unwrap();
        }
        rig.host.start().unwrap();
        rig.run(0.0, 4);
        let server = usize::from(role != ScriptRole::Client);
        let client = usize::from(role != ScriptRole::Server);
        assert_eq!(
            count(&rig, "print:shared loaded"),
            server + client,
            "{role:?}: {:?}",
            rig.lines()
        );
        assert_eq!(count(&rig, "print:server cached true"), server);
        assert_eq!(count(&rig, "print:server secret secret"), server);
        assert_eq!(count(&rig, "print:client cached true"), client);
        assert_eq!(count(&rig, "print:client blocked true"), client);
        assert_eq!(
            rig.lines().len(),
            3 * server + 3 * client,
            "{:?}",
            rig.lines()
        );
    }
}

#[test]
fn completed_modules_survive_client_rebind_but_not_vm_restart() {
    let mut rig = rig(ScriptRole::Client);
    let first = player(&rig, "First");
    let second = player(&rig, "Second");
    module(
        &rig,
        "Shared",
        "print('loaded') return function() return game.Players.LocalPlayer.Name end",
    );
    rig.local_script(
        "Client",
        "print('bound', require(game.ReplicatedStorage.Shared)())",
    );
    rig.host.set_local_player(first).unwrap();
    rig.host.start().unwrap();
    let now = rig.run(0.0, 2);
    assert_eq!(rig.lines(), ["print:loaded", "print:bound First"]);
    rig.host.set_local_player(second).unwrap();
    let now = rig.run(now, 2);
    assert_eq!(
        rig.lines(),
        ["print:loaded", "print:bound First", "print:bound Second"]
    );
    rig.host.stop();
    rig.host.start().unwrap();
    rig.run(now, 2);
    assert_eq!(
        count(&rig, "print:loaded"),
        2,
        "a restarted VM has a fresh module cache"
    );
    assert_eq!(count(&rig, "print:bound Second"), 2);
    assert_eq!(rig.lines().len(), 5, "{:?}", rig.lines());
}

#[test]
fn rebinding_releases_a_suspended_module_loader_without_waking_its_old_owner() {
    let mut rig = rig(ScriptRole::Client);
    let first = player(&rig, "First");
    let second = player(&rig, "Second");
    let slow = module(
        &rig,
        "Slow",
        "print('loading', game.Players.LocalPlayer.Name) task.wait(1000) print('stale module') return 'stale'",
    );
    rig.local_script(
        "Client",
        "print('got', require(game.ReplicatedStorage.Slow))",
    );
    rig.host.set_local_player(first).unwrap();
    rig.host.start().unwrap();
    let now = rig.run(0.0, 3);
    assert_eq!(rig.lines(), ["print:loading First"]);
    rig.world
        .borrow_mut()
        .set(slow, "Source", Value::String("return 'fresh'".into()))
        .unwrap();
    rig.host.set_local_player(second).unwrap();
    let now = rig.run(now, 3);
    assert_eq!(rig.lines(), ["print:loading First", "print:got fresh"]);
    rig.run(now + 1001.0, 2);
    assert_eq!(rig.lines(), ["print:loading First", "print:got fresh"]);
}

#[test]
fn transferring_a_client_owner_releases_its_module_for_an_eligible_waiter() {
    let mut rig = rig(ScriptRole::Client);
    let local = player(&rig, "Local");
    let other = player(&rig, "Other");
    let own_gui = rig.add("PlayerGui", "PlayerGui", local);
    let other_gui = rig.add("PlayerGui", "PlayerGui", other);
    let slow = module(
        &rig,
        "Slow",
        "print('loader started') task.wait(1000) print('stale module') return 'stale'",
    );
    let loader = rig.add_script(
        "LocalScript",
        "Loader",
        own_gui,
        "print('old owner', require(game.ReplicatedStorage.Slow))",
    );
    rig.local_script(
        "Waiter",
        "task.wait(0.1) print('waiter', require(game.ReplicatedStorage.Slow))",
    );
    rig.host.set_local_player(local).unwrap();
    rig.host.start().unwrap();
    let now = rig.run(0.0, 20);
    assert_eq!(rig.lines(), ["print:loader started"]);
    {
        let mut world = rig.world.borrow_mut();
        world
            .set(slow, "Source", Value::String("return 'fresh'".into()))
            .unwrap();
        world.set_parent(loader, Some(other_gui)).unwrap();
    }
    let now = rig.run(now, 5);
    assert_eq!(rig.lines(), ["print:loader started", "print:waiter fresh"]);
    rig.run(now + 1001.0, 2);
    assert_eq!(rig.lines(), ["print:loader started", "print:waiter fresh"]);
}
