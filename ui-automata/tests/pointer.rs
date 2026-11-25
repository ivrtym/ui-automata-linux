use std::collections::HashMap;

use ui_automata::{
    Executor,
    lint::lint,
    mock::{MockDesktop, MockElement},
    yaml::WorkflowFile,
};

#[test]
fn pointer_scroll_supports_element_center_and_screen_coords() {
    let list = MockElement::parent("list", "通讯录", vec![]).with_bounds(140, 380, 240, 700);
    let win = MockElement::parent("window", "微信", vec![list]).with_bounds(80, 300, 920, 800);
    let desktop = MockDesktop::new(vec![win]);
    let scrolls = desktop.pointer_scrolls.clone();

    let yaml = r#"
name: test_pointer_scroll
anchors:
  wechat:
    type: Root
    selector: "[role=window]"
phases:
  - name: run
    mount: [wechat]
    steps:
      - intent: scroll contacts list down
        action:
          type: PointerScroll
          scope: wechat
          selector: ">> [role=list][name=通讯录]"
          delta_y: -5
        expect:
          type: Always
      - intent: scroll at screen point
        action:
          type: PointerScroll
          x: 500
          y: 600
          delta_x: 2
          delta_y: 3
        expect:
          type: Always
"#;

    assert!(lint(yaml).is_empty());
    let wf = WorkflowFile::load_from_str(yaml, &HashMap::new()).unwrap();
    let mut ex = Executor::new(desktop);
    wf.run(&mut ex, None, None).unwrap();

    let recorded = scrolls.lock().unwrap();
    assert_eq!(*recorded, vec![(260, 730, 0, -5), (500, 600, 2, 3)]);
}

#[test]
fn pointer_drag_supports_two_elements_and_screen_coords() {
    let src = MockElement::leaf("thumb", "滑块").with_bounds(100, 200, 40, 40);
    let dst = MockElement::leaf("target", "终点").with_bounds(300, 200, 60, 40);
    let win = MockElement::parent("window", "App", vec![src, dst]).with_bounds(50, 50, 500, 400);
    let desktop = MockDesktop::new(vec![win]);
    let drags = desktop.pointer_drags.clone();

    let yaml = r#"
name: test_pointer_drag
anchors:
  app:
    type: Root
    selector: "[role=window]"
phases:
  - name: run
    mount: [app]
    steps:
      - intent: drag slider to target element
        action:
          type: PointerDrag
          scope: app
          selector: ">> [role=thumb]"
          to_selector: ">> [role=target]"
        expect:
          type: Always
      - intent: drag by window-relative coordinates
        action:
          type: PointerDrag
          scope: app
          from_x: 10
          from_y: 20
          to_x: 110
          to_y: 120
        expect:
          type: Always
"#;

    assert!(lint(yaml).is_empty());
    let wf = WorkflowFile::load_from_str(yaml, &HashMap::new()).unwrap();
    let mut ex = Executor::new(desktop);
    wf.run(&mut ex, None, None).unwrap();

    let recorded = drags.lock().unwrap();
    assert_eq!(
        *recorded,
        vec![(120, 220, 330, 220), (60, 70, 160, 170)]
    );
}

#[test]
fn press_key_allows_scope_only_and_global_shortcuts() {
    let win = MockElement::parent("window", "微信", vec![]);
    let desktop = MockDesktop::new(vec![win]);
    let keys = desktop.pointer_keys.clone();

    let yaml = r#"
name: test_press_key_modes
anchors:
  wechat:
    type: Root
    selector: "[role=window]"
phases:
  - name: run
    mount: [wechat]
    steps:
      - intent: press ctrl+f scoped to window without selector
        action:
          type: PressKey
          scope: wechat
          key: "ctrl+f"
        expect:
          type: Always
      - intent: press global escape without scope or selector
        action:
          type: PressKey
          key: "{ESCAPE}"
        expect:
          type: Always
"#;

    assert!(lint(yaml).is_empty());
    let wf = WorkflowFile::load_from_str(yaml, &HashMap::new()).unwrap();
    let mut ex = Executor::new(desktop);
    wf.run(&mut ex, None, None).unwrap();

    let recorded = keys.lock().unwrap();
    assert_eq!(*recorded, vec!["{ESCAPE}"]);
}

#[test]
fn lint_validates_pointer_actions_and_press_key_rules() {
    let bad = r#"
name: bad_pointer_actions
phases:
  - name: p
    steps:
      - intent: scroll missing delta and coords
        action:
          type: PointerScroll
        expect:
          type: Always
      - intent: drag missing dest and coords
        action:
          type: PointerDrag
        expect:
          type: Always
      - intent: press key selector without scope
        action:
          type: PressKey
          selector: ">> [role=button]"
          key: Return
        expect:
          type: Always
"#;

    let diags = lint(bad);
    let msgs: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
    assert!(msgs.iter().any(|m| m.contains("both 'x' and 'y'")));
    assert!(msgs.iter().any(|m| m.contains("at least one of 'delta_x' or 'delta_y'")));
    assert!(msgs.iter().any(|m| m.contains("'from_x' and 'from_y'")));
    assert!(msgs.iter().any(|m| m.contains("requires 'to_selector' or both 'to_x' and 'to_y'")));
    assert!(msgs.iter().any(|m| m.contains("'selector' requires 'scope'")));
}
