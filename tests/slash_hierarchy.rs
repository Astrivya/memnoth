use memnoth::tui::slash::SlashMenu;

#[test]
fn slash_menu_collapses_families_and_expands_the_selected_root() {
    let mut menu = SlashMenu::new();
    menu.sync("/", 1);

    let roots = menu.rows();
    assert!(roots.iter().all(|row| !row.name[1..].contains(' ')));
    let telemetry = roots
        .iter()
        .position(|row| row.name == "/telemetry")
        .expect("telemetry should be discoverable at the root");
    for _ in 0..telemetry {
        menu.select_next();
    }

    assert_eq!(menu.accept(), Some(("/telemetry ".into(), false)));
    menu.sync("/telemetry ", "/telemetry ".chars().count());
    let children: Vec<_> = menu.rows().into_iter().map(|row| row.name).collect();
    for action in ["status", "on", "off"] {
        assert!(children.iter().any(|child| child == action), "{action}");
    }
}
