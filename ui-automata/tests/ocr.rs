mod common;

use std::collections::HashMap;

use ui_automata::lint::lint;
use ui_automata::mock::{MockDesktop, MockElement};
use ui_automata::yaml::WorkflowFile;
use ui_automata::{ClickType, Executor, OcrMatch};

fn sample_ocr_blocks() -> Vec<OcrMatch> {
    vec![
        // Low-confidence decoy that should be filtered out when min_confidence: 0.8
        OcrMatch {
            text: "Confirm".into(),
            confidence: 0.45,
            x: 10,
            y: 10,
            width: 60,
            height: 20,
        },
        // Match #0 above threshold
        OcrMatch {
            text: "Confirm".into(),
            confidence: 0.92,
            x: 100,
            y: 50,
            width: 80,
            height: 20,
        },
        // Match #1 above threshold
        OcrMatch {
            text: "Confirm".into(),
            confidence: 0.95,
            x: 100,
            y: 150,
            width: 80,
            height: 20,
        },
        OcrMatch {
            text: "Username:".into(),
            confidence: 0.91,
            x: 50,
            y: 220,
            width: 90,
            height: 20,
        },
    ]
}

#[test]
fn ocr_click_filters_confidence_nth_and_offset() {
    let yaml = r#"
name: ocr_click_test
phases:
  - name: main
    steps:
      - intent: double click second high-confidence Confirm with offset
        action:
          type: OcrClick
          pattern:
            exact: "Confirm"
          kind: double
          nth: 1
          min_confidence: 0.8
          offset_x: 30
          offset_y: -5
        expect:
          type: Always
"#;

    let desktop = MockDesktop::new(vec![]).with_ocr(sample_ocr_blocks());
    let clicks = desktop.pointer_clicks.clone();
    let (res, _) = common::run(yaml, desktop);
    assert!(res.is_ok(), "{res:?}");
    let clicks = clicks.lock().unwrap();
    // Block #1: x=100, y=150, w=80, h=20 -> center=(140,160) + (30,-5) = (170,155)
    assert_eq!(*clicks, vec![(170, 155, ClickType::Double)]);
}

#[test]
fn ocr_hover_type_extract_and_screenshot_pipeline() {
    let yaml = r#"
name: ocr_pipeline_test
outputs:
  - name: found_items
phases:
  - name: main
    steps:
      - intent: extract all matches
        action:
          type: OcrExtract
          key: found_items
          pattern:
            contains: "Confirm"
          multiple: true
          min_confidence: 0.8
        expect:
          type: Always
      - intent: hover first extracted label
        action:
          type: OcrHover
          pattern:
            exact: "{output.found_items}"
          min_confidence: 0.8
        expect:
          type: Always
      - intent: type into field next to Username label
        action:
          type: OcrTypeText
          pattern:
            starts_with: "Username"
          offset_x: 120
          text: "alice-{output.found_items}"
        expect:
          type: Always
      - intent: capture screenshot with output name
        action:
          type: Screenshot
          path: "/tmp/shot_{output.found_items}.png"
        expect:
          type: Always
"#;

    let desktop = MockDesktop::new(vec![]).with_ocr(sample_ocr_blocks());
    let moves = desktop.pointer_moves.clone();
    let clicks = desktop.pointer_clicks.clone();
    let types = desktop.pointer_types.clone();
    let shots = desktop.screenshots.clone();

    let (res, _, out) = common::run_full(yaml, desktop);
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(
        out.as_map().get("found_items").cloned().unwrap_or_default(),
        vec!["Confirm".to_string(), "Confirm".to_string()]
    );
    assert_eq!(*moves.lock().unwrap(), vec![(140, 60)]);
    // Username center (95, 230) + offset_x 120 => (215, 230)
    assert_eq!(*clicks.lock().unwrap(), vec![(215, 230, ClickType::Left)]);
    assert_eq!(*types.lock().unwrap(), vec!["alice-Confirm".to_string()]);
    assert_eq!(
        *shots.lock().unwrap(),
        vec![(None, None, "/tmp/shot_Confirm.png".to_string())]
    );
}

#[test]
fn ocr_text_found_respects_selector_element_bounds() {
    let canvas = MockElement::leaf("pane", "Canvas").with_bounds(80, 130, 150, 70);
    let win = MockElement::parent("window", "App", vec![canvas]);

    let yaml = r#"
name: ocr_selector_crop
anchors:
  app:
    type: Root
    selector: "[name=App]"
phases:
  - name: main
    mount: [app]
    steps:
      - intent: verify text inside canvas bounds
        action:
          type: NoOp
        expect:
          type: OcrTextFound
          scope: app
          selector: ">> [role=pane][name=Canvas]"
          pattern:
            exact: "Confirm"
          min_confidence: 0.9
"#;

    let desktop = MockDesktop::new(vec![win]).with_ocr(sample_ocr_blocks());
    let ocr_calls = desktop.ocr_calls.clone();
    let (res, _) = common::run(yaml, desktop);
    assert!(res.is_ok(), "{res:?}");
    let calls = ocr_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    let (_, region) = calls[0];
    let r = region.expect("expected selector crop region");
    assert_eq!((r.x, r.y, r.width, r.height), (80, 130, 150, 70));
}

#[test]
fn ocr_click_works_as_step_fallback_with_param_substitution() {
    // Window has NO "Submit" button in accessibility tree, so Invoke fails,
    // and fallback OcrClick executes instead with {param.btn} substituted.
    let win = MockElement::parent("window", "App", vec![]);
    let ocr_matches = vec![OcrMatch {
        text: "Submit".into(),
        confidence: 0.98,
        x: 200,
        y: 300,
        width: 80,
        height: 30,
    }];

    let yaml = r#"
name: ocr_fallback_test
params:
  - name: btn
    default: "Submit"
defaults:
  timeout: 150ms
anchors:
  app:
    type: Root
    selector: "[name=App]"
phases:
  - name: main
    mount: [app]
    steps:
      - intent: invoke Submit or fallback to OCR click
        action:
          type: Invoke
          scope: app
          selector: ">> [role=button][name={param.btn}]"
        fallback:
          type: OcrClick
          pattern:
            exact: "{param.btn}"
        expect:
          type: OcrTextFound
          pattern:
            exact: "{param.btn}"
"#;

    let mut params = HashMap::new();
    params.insert("btn".into(), "Submit".into());
    let wf = WorkflowFile::load_from_str(yaml, &params).unwrap();
    let desktop = MockDesktop::new(vec![win]).with_ocr(ocr_matches);
    let clicks = desktop.pointer_clicks.clone();
    let mut executor = Executor::new(desktop);

    let res = wf.run(&mut executor, None, None);
    assert!(res.is_ok(), "{res:?}");
    assert_eq!(*clicks.lock().unwrap(), vec![(240, 315, ClickType::Left)]);
}

#[test]
fn lint_validates_ocr_actions_and_conditions() {
    let valid = r#"
name: ocr_lint_ok
anchors:
  app:
    type: Root
    selector: "[name=App]"
phases:
  - name: main
    mount: [app]
    steps:
      - intent: ocr click and check
        action:
          type: OcrClick
          scope: app
          selector: ">> [role=pane]"
          pattern:
            contains: "Save"
          kind: left
        fallback:
          type: Screenshot
          path: "/tmp/fail.png"
        expect:
          type: OcrTextFound
          pattern:
            non_empty: true
"#;
    let diags = lint(valid);
    assert!(diags.is_empty(), "unexpected diags: {diags:#?}");

    let invalid = r#"
name: ocr_lint_err
phases:
  - name: main
    steps:
      - intent: bad selector without scope
        action:
          type: OcrClick
          selector: ">> [role=pane]"
          pattern:
            contains: "Save"
          kind: bad_kind
        expect:
          type: OcrTextFound
"#;
    let diags = lint(invalid);
    let msgs: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
    assert!(msgs.iter().any(|m| m.contains("requires 'scope'")), "{msgs:?}");
    assert!(msgs.iter().any(|m| m.contains("unknown click kind")), "{msgs:?}");
    assert!(msgs.iter().any(|m| m.contains("missing required field 'pattern'")), "{msgs:?}");
}
