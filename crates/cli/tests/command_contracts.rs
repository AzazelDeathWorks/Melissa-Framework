use melissa_cli::{
    args::{self, Command},
    display,
};
use serde_json::json;

fn parse(a: &[&str]) -> melissa_cli::Result<args::Options> {
    args::parse(a.iter().map(std::ffi::OsString::from))
}

#[test]
fn preparation_memory_and_replanning_commands_are_explicit_and_do_not_alias_run() {
    assert!(matches!(
        parse(&[
            "task",
            "prepare",
            "--workspace",
            "project",
            "--goal",
            "hatayı düzelt",
            "--out",
            "private.json"
        ])
        .unwrap()
        .command,
        Command::Prepare { .. }
    ));
    assert!(matches!(
        parse(&[
            "memory",
            "accept",
            "--workspace",
            "project",
            "--id",
            "m1",
            "--digest",
            "review"
        ])
        .unwrap()
        .command,
        Command::Memory { .. }
    ));
    assert!(matches!(
        parse(&["task", "replan", "task_a", "--recipe", "private.json"])
            .unwrap()
            .command,
        Command::Replan { .. }
    ));
    for a in [
        vec!["task", "prepare", "--workspace", "project", "--goal", "fix"],
        vec!["task", "replan", "task_a", "--recipe", "a", "--recipe", "b"],
        vec![
            "memory",
            "list",
            "--workspace",
            "project",
            "--execute",
            "yes",
        ],
    ] {
        assert!(parse(&a).is_err());
    }
    assert!(args::CAPABILITIES_HELP.contains("modeli çalıştırmaz"));
}

#[test]
fn strict_parser_accepts_explicit_workspace_and_unicode_goal() {
    let o = parse(&[
        "--home",
        "state space",
        "--json",
        "task",
        "new",
        "--workspace",
        "Türkçe proje",
        "--goal",
        "Hataları düzelt",
        "--exclude",
        "out",
        "--exclude",
        "out",
    ])
    .unwrap();
    assert!(o.json);
    assert_eq!(o.home.unwrap().to_str().unwrap(), "state space");
    let Command::New {
        workspace,
        goal,
        profile,
        excludes,
        recipe,
    } = o.command
    else {
        panic!()
    };
    assert_eq!(workspace.to_str().unwrap(), "Türkçe proje");
    assert_eq!(goal, "Hataları düzelt");
    assert_eq!(profile, "melissa.code.implementer");
    assert_eq!(excludes, ["out", "out"]);
    assert!(recipe.is_none());
}

#[test]
fn ambiguous_unknown_and_incomplete_commands_are_rejected() {
    for a in [
        vec!["--home"],
        vec!["--json", "--json", "init"],
        vec!["--home", "a", "--home", "b", "init"],
        vec!["task"],
        vec!["task", "unknown-action", "x"],
        vec!["--provider", "x"],
        vec!["task", "new", "--workspace", "x"],
        vec![
            "task",
            "new",
            "--workspace",
            "x",
            "--goal",
            "a",
            "--goal",
            "b",
        ],
        vec!["task", "show", "x", "extra"],
        vec!["task", "approve", "x", "--request", "r"],
        vec![
            "task",
            "approve",
            "x",
            "--request",
            "r",
            "--request",
            "r",
            "--digest",
            "d",
        ],
        vec!["task", "evidence", "x", "--artifact", "a"],
        vec!["task", "evidence", "x", "--out", "a"],
        vec!["task", "list", "--json"],
    ] {
        assert!(parse(&a).is_err(), "{a:?}");
    }
}

#[test]
fn help_version_and_evidence_export_are_explicit() {
    assert!(matches!(parse(&[]).unwrap().command, Command::Help));
    assert!(matches!(
        parse(&["--version"]).unwrap().command,
        Command::Version
    ));
    let c = parse(&[
        "task",
        "evidence",
        "id",
        "--artifact",
        "a",
        "--out",
        "new.json",
    ])
    .unwrap()
    .command;
    assert!(matches!(
        c,
        Command::Evidence {
            artifact: Some(_),
            out: Some(_),
            ..
        }
    ));
    assert!(args::HELP.contains("Tarifsiz görev yalnız incelemedir"));
}
#[test]
fn preview_requires_explicit_new_export_destination_and_has_no_server_start_flag() {
    assert!(matches!(
        parse(&["task", "preview", "id"]).unwrap().command,
        Command::Preview { out: None, .. }
    ));
    assert!(matches!(
        parse(&["task", "preview", "id", "--out", "new images"])
            .unwrap()
            .command,
        Command::Preview { out: Some(_), .. }
    ));
    for args in [
        vec!["task", "preview"],
        vec!["task", "preview", "id", "--out"],
        vec!["task", "preview", "id", "--serve"],
        vec!["task", "preview", "id", "--out", "a", "--out", "b"],
    ] {
        assert!(parse(&args).is_err());
    }
    assert!(args::DOMAIN_HELP.contains("task preview"));
}

#[test]
fn terminal_control_and_bidi_are_escaped_without_changing_json_data() {
    let text = "görev\n\u{1b}[2J\u{202e}fake\u{2066}";
    let v = json!({"task_id":"task_x", "goal":text,"state":"\u{1b}[31m"});
    let encoded = display::json(&v).unwrap();
    assert!(!encoded.contains('\u{1b}') && !encoded.contains('\u{202e}'));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
        v
    );
    let human = display::human(&v).unwrap();
    assert!(!human.contains('\u{1b}') && !human.contains('\u{202e}'));
    assert!(human.contains("\\u{a}"));
}
