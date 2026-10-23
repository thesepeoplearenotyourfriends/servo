/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Native document selection and the trusted embedder Copy action.
mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use servo::{
    ClipboardDelegate, EditingActionEvent, InputEvent, JSValue, MouseButton, MouseButtonAction,
    MouseButtonEvent, MouseMoveEvent, WebView, WebViewBuilder,
};
use url::Url;
use webrender_api::units::DevicePoint;

use common::{
    ServoTest, WebViewDelegateImpl, evaluate_javascript,
    show_webview_and_wait_for_rendering_to_be_ready,
};

#[derive(Default)]
struct ClipboardRecorder(RefCell<Option<String>>);

impl ClipboardDelegate for ClipboardRecorder {
    fn set_text(&self, _webview: WebView, text: String) {
        *self.0.borrow_mut() = Some(text);
    }
}

fn wait_for_scene(servo_test: &ServoTest, webview: &WebView) {
    let done = Rc::new(Cell::new(false));
    let captured = done.clone();
    webview.take_screenshot(None, move |result| {
        assert!(result.is_ok());
        captured.set(true);
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let waiting = done.clone();
    servo_test.spin(move || !waiting.get() && Instant::now() < deadline);
    assert!(done.get(), "Native input scene did not become ready");
}

fn copy(servo_test: &ServoTest, webview: &WebView, clipboard: &Rc<ClipboardRecorder>) -> String {
    *clipboard.0.borrow_mut() = None;
    webview.notify_input_event(InputEvent::EditingAction(EditingActionEvent::Copy));
    let captured = clipboard.clone();
    let deadline = Instant::now() + Duration::from_secs(10);
    servo_test.spin(move || captured.0.borrow().is_none() && Instant::now() < deadline);
    clipboard
        .0
        .borrow()
        .clone()
        .expect("Copy did not reach the clipboard delegate")
}

// Unlike rAF, this waits for an up-to-date WebRender scene with no pending frames.
fn scene_pixels(servo_test: &ServoTest, webview: &WebView) -> Vec<u8> {
    let pixels = Rc::new(RefCell::new(None));
    let captured = pixels.clone();
    webview.take_screenshot(None, move |result| {
        *captured.borrow_mut() = Some(result.expect("Selection screenshot failed").into_raw());
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let waiting = pixels.clone();
    servo_test.spin(move || waiting.borrow().is_none() && Instant::now() < deadline);
    pixels
        .borrow_mut()
        .take()
        .expect("Selection scene timed out")
}

#[test]
fn document_selection_replacement_collapse_and_clear_remove_old_pixels() {
    let servo_test = ServoTest::new();
    let delegate = Rc::new(WebViewDelegateImpl::default());
    let clipboard = Rc::new(ClipboardRecorder::default());
    let url = Url::parse(concat!(
        "data:text/html,<!doctype html><style>",
        "body,p{margin:0}p{width:12ch;font:20px/24px monospace}",
        "::selection{background:rgb(176,214,255)}</style>",
        "<p id=one>alpha bravo charlie delta</p>",
        "<p id=wrapped>echo <span>foxtrot <b>golf hotel</b> india</span> juliet</p>",
        "<p id=last>kilo lima november</p>",
        "<script>",
        "function select(start, a, end, b) {",
        "const r = document.createRange(); r.setStart(start,a); r.setEnd(end,b);",
        "getSelection().removeAllRanges(); getSelection().addRange(r);}",
        "const first = one.firstChild, final = last.firstChild;",
        "function early() {select(first,0,first,5)}",
        "function late() {select(first,20,first,25)}",
        "function other() {select(final,5,final,9)}",
        "function nested() {select(wrapped.firstChild,1,wrapped.lastChild,7)}",
        "</script>"
    ))
    .unwrap();
    let webview = WebViewBuilder::new(servo_test.servo(), servo_test.rendering_context.clone())
        .delegate(delegate.clone())
        .clipboard_delegate(clipboard.clone())
        .url(url)
        .build();
    show_webview_and_wait_for_rendering_to_be_ready(&servo_test, &webview, &delegate);
    let run = |script| evaluate_javascript(&servo_test, webview.clone(), script).unwrap();
    let assert_selection = |expected: &str| {
        assert_eq!(
            run("getSelection().toString()"),
            JSValue::String(expected.into())
        );
    };
    let unselected = scene_pixels(&servo_test, &webview);

    // Directly selecting B gives a pixel oracle independent of replacement.
    run("late()");
    let late = scene_pixels(&servo_test, &webview);
    assert!(late != unselected, "The oracle must contain a highlight");
    run("early()");
    wait_for_scene(&servo_test, &webview);
    run("late()");
    assert_selection("delta");
    assert!(
        scene_pixels(&servo_test, &webview) == late,
        "Disjoint offsets"
    );
    assert_eq!(copy(&servo_test, &webview, &clipboard), "delta");

    run("getSelection().removeAllRanges()");
    assert_eq!(run("getSelection().rangeCount"), JSValue::Number(0.0));
    assert!(scene_pixels(&servo_test, &webview) == unselected, "Clear");
    run("other()");
    let other = scene_pixels(&servo_test, &webview);
    assert!(other != unselected, "The oracle must contain a highlight");
    run("early()");
    wait_for_scene(&servo_test, &webview);
    run("other()");
    assert_selection("lima");
    assert!(
        scene_pixels(&servo_test, &webview) == other,
        "Different nodes"
    );
    assert_eq!(copy(&servo_test, &webview, &clipboard), "lima");

    run("getSelection().collapse(final,5)");
    assert_eq!(run("getSelection().isCollapsed"), JSValue::Boolean(true));
    assert!(
        scene_pixels(&servo_test, &webview) == unselected,
        "Collapse"
    );
    run("nested()");
    assert_selection("cho foxtrot golf hotel india juliet");
    assert!(
        scene_pixels(&servo_test, &webview) != unselected,
        "Nested range"
    );
    run("other()");
    assert_selection("lima");
    assert!(
        scene_pixels(&servo_test, &webview) == other,
        "Wrapped nested range"
    );

    // Coalesced changes, followed by changes across completed scenes.
    run("early(); nested(); getSelection().removeAllRanges(); late(); other()");
    assert_selection("lima");
    assert!(
        scene_pixels(&servo_test, &webview) == other,
        "Coalesced changes"
    );
    for _ in 0..3 {
        run("nested(); early()");
        wait_for_scene(&servo_test, &webview);
        run("late(); other()");
        assert_selection("lima");
        assert!(
            scene_pixels(&servo_test, &webview) == other,
            "Successive scenes"
        );
    }
    run("getSelection().removeAllRanges()");
    assert_selection("");
    assert!(
        scene_pixels(&servo_test, &webview) == unselected,
        "Final clear"
    );
}

#[test]
fn native_document_drag_updates_selection_and_copy_preserves_text_controls() {
    let servo_test = ServoTest::new();
    let delegate = Rc::new(WebViewDelegateImpl::default());
    let clipboard = Rc::new(ClipboardRecorder::default());
    let webview = WebViewBuilder::new(servo_test.servo(), servo_test.rendering_context.clone())
        .delegate(delegate.clone())
        .clipboard_delegate(clipboard.clone())
        .url(Url::parse("data:text/html,<!doctype html><style>body,p{margin:0}p{font:20px/24px monospace}</style><p>selection target</p><input id=editor value=control>").unwrap())
        .build();
    show_webview_and_wait_for_rendering_to_be_ready(&servo_test, &webview, &delegate);

    let start = DevicePoint::new(2.0, 10.0).into();
    let end = DevicePoint::new(120.0, 10.0).into();
    // Queue a whole drag before spinning: coalescing must not move the second
    // motion in front of mousedown, or the document range will remain collapsed.
    webview.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(start)));
    webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
        MouseButtonAction::Down,
        MouseButton::Left,
        start,
    )));
    webview.notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(end)));
    wait_for_scene(&servo_test, &webview);
    let JSValue::String(selected) = evaluate_javascript(
        &servo_test,
        webview.clone(),
        "window.getSelection().toString()",
    )
    .unwrap() else {
        panic!("Selection stringifier did not return text");
    };
    assert!(
        !selected.is_empty(),
        "A native drag must select ordinary paragraph text"
    );
    assert!("selection target".contains(selected.as_str()));

    webview.notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
        MouseButtonAction::Up,
        MouseButton::Left,
        end,
    )));
    wait_for_scene(&servo_test, &webview);
    assert_eq!(
        evaluate_javascript(
            &servo_test,
            webview.clone(),
            "window.getSelection().toString()"
        ),
        Ok(JSValue::String(selected.clone()))
    );
    assert_eq!(copy(&servo_test, &webview, &clipboard), selected);

    // The document still has its range, but a focused text control's own copy
    // implementation must win over the new document-copy default action.
    evaluate_javascript(
        &servo_test,
        webview.clone(),
        "editor.focus(); editor.setSelectionRange(1, 4);",
    )
    .unwrap();
    assert_eq!(copy(&servo_test, &webview, &clipboard), "ont");
}
