use std::collections::VecDeque;
use std::sync::Arc;

use gpui::{AppContext as _, ClipboardItem, TestAppContext};
use nyaterm_core::ActionLinksMatcherSettings;
use nyaterm_terminal::TerminalSnapshot;
use nyaterm_terminal::selection::{TerminalBufferCellPos, TerminalSelection};
use nyaterm_ui::NyaMenuItem;

use crate::features::NyaTermApp;
use crate::features::terminal::state::TerminalSelectionTextStatus;
use crate::features::terminal::terminal_surface::terminal_snapshot_absolute_range;
use crate::features::test_support::app_with_visible_local_session;
use crate::models::{TerminalFrameEvent, TerminalSelectionTextError};
use crate::test_support::{TestAppCleanup, TestConfigDir};

fn copy_item_disabled(items: &[NyaMenuItem]) -> bool {
    items
        .iter()
        .find(|item| item.test_label() == "Copy")
        .expect("Copy item")
        .test_presentation()
        .3
}

fn seed_full_scrollback_session(app: &mut NyaTermApp) -> Arc<TerminalSnapshot> {
    let mut settings = app.settings.summary().clone();
    settings.terminal_scrollback_lines = 100;
    app.settings.replace_summary(settings);
    let output = (0..150)
        .map(|i| format!("line {i:04}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let pipeline = &app.terminal.view.frame_pipeline;
    pipeline.seed_session("s1", output, "UTF-8", 100);
    pipeline.request_snapshot("s1", 0, false, ActionLinksMatcherSettings::default());
    pipeline.flush_for_test();
    let mut events = VecDeque::new();
    pipeline.drain_events_into(&mut events, usize::MAX);
    events
        .into_iter()
        .find_map(|event| match event {
            TerminalFrameEvent::Snapshot(frame) => Some(frame.snapshot),
            _ => None,
        })
        .expect("worker snapshot")
}

fn push_lines_and_apply(app: &mut NyaTermApp, cx: &mut gpui::Context<NyaTermApp>, lines: usize) {
    let mut data = String::new();
    for i in 0..lines {
        data.push_str(&format!("\r\nextra {i:04}"));
    }
    app.submit_terminal_frame_output("s1", data.into_bytes());
    app.terminal.view.frame_pipeline.request_snapshot(
        "s1",
        0,
        false,
        ActionLinksMatcherSettings::default(),
    );
    app.terminal.view.frame_pipeline.flush_for_test();
    while app.terminal.view.frame_pipeline.queued_event_count() > 0
        || !app.terminal.view.pending_frame_events.is_empty()
    {
        app.drain_terminal_frame_events(cx);
    }
}

fn seed_filled_scrollback_session(
    app: &mut NyaTermApp,
    lines: usize,
    limit: u32,
) -> Arc<TerminalSnapshot> {
    let mut settings = app.settings.summary().clone();
    settings.terminal_scrollback_lines = limit;
    app.settings.replace_summary(settings);
    let output = (0..lines)
        .map(|i| format!("line {i:04}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let pipeline = &app.terminal.view.frame_pipeline;
    pipeline.seed_session("s1", output, "UTF-8", limit as usize);
    pipeline.request_snapshot("s1", 0, false, ActionLinksMatcherSettings::default());
    pipeline.flush_for_test();
    let mut events = VecDeque::new();
    pipeline.drain_events_into(&mut events, usize::MAX);
    events
        .into_iter()
        .find_map(|event| match event {
            TerminalFrameEvent::Snapshot(frame) => Some(frame.snapshot),
            _ => None,
        })
        .expect("worker snapshot")
}

fn seed_cross_screen_selection(app: &mut NyaTermApp) {
    let output = (0..600)
        .map(|i| format!("line {i:04}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let pipeline = &app.terminal.view.frame_pipeline;
    pipeline.seed_session("s1", output, "UTF-8", 1000);
    pipeline.request_snapshot("s1", 0, false, ActionLinksMatcherSettings::default());
    pipeline.flush_for_test();
    let mut events = VecDeque::new();
    pipeline.drain_events_into(&mut events, usize::MAX);
    let snapshot = events
        .into_iter()
        .find_map(|event| match event {
            TerminalFrameEvent::Snapshot(frame) => Some(frame.snapshot),
            _ => None,
        })
        .expect("worker snapshot");
    assert!(snapshot.row_count() < 500);
    app.terminal.selection.source_history = Some(snapshot.history);
    let view = app.terminal.view.views.get_mut("s1").unwrap();
    assert!(view.screen.all_text().is_none());
    view.frame_snapshot = Some(snapshot);
    app.terminal.selection.session_id = Some("s1".into());
    app.terminal.selection.selection = Some(TerminalSelection::from_range(
        TerminalBufferCellPos::new(0, 0),
        TerminalBufferCellPos::new(499, 8),
    ));
}

#[test]
fn selection_survives_scrollback_eviction_and_copy_stays_enabled() {
    let dir = TestConfigDir::new("nyaterm-selection-eviction");
    let mut cx = TestAppContext::single();
    let _cleanup = TestAppCleanup::new(&cx);
    let app = app_with_visible_local_session(&mut cx, dir.path(), "s1");
    let (expected, initial_line, evicted) = cx.update_entity(&app, |app, cx| {
        let snapshot = seed_full_scrollback_session(app);
        let (absolute_start, _) = terminal_snapshot_absolute_range(&snapshot);
        let selection = TerminalSelection::from_range(
            TerminalBufferCellPos::new(absolute_start + 1, 0),
            TerminalBufferCellPos::new(absolute_start + 2, 8),
        );
        let view = app.terminal.view.views.get_mut("s1").unwrap();
        view.frame_snapshot = Some(snapshot.clone());
        app.terminal.selection.session_id = Some("s1".into());
        app.terminal.selection.selection = Some(selection);
        app.terminal.selection.source_history = Some(snapshot.history);
        let expected = app.selected_terminal_text().expect("selected text");
        let initial_line = selection.anchor.position.line;

        let previous = app.terminal.view.views.get("s1").unwrap().history_state();
        push_lines_and_apply(app, cx, 8);
        let current = app.terminal.view.views.get("s1").unwrap().history_state();
        let shift = current.reanchor_shift(previous).expect("history retained");
        let growth = current
            .scrollback_len
            .saturating_sub(previous.scrollback_len);
        let evicted = shift.saturating_sub(growth);
        assert!(evicted > 0, "the scrollback buffer must have been full");
        assert_eq!(
            app.terminal
                .selection
                .selection
                .expect("selection survives")
                .anchor
                .position
                .line,
            initial_line - evicted
        );
        assert_eq!(app.terminal.selection.source_history, Some(current));
        (expected, initial_line, evicted)
    });
    cx.update_entity(&app, |app, cx| {
        let items = app.terminal_context_menu_items_for_session("s1".into(), cx);
        assert!(
            !copy_item_disabled(&items),
            "Copy must stay enabled after eviction"
        );
        app.terminal.view.frame_pipeline.flush_for_test();
    });
    cx.run_until_parked();
    cx.update_entity(&app, |app, cx| {
        assert_eq!(
            app.selected_terminal_text().as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            app.terminal
                .selection
                .selection
                .expect("selection survives")
                .anchor
                .position
                .line,
            initial_line - evicted
        );
        let items = app.terminal_context_menu_items_for_session("s1".into(), cx);
        assert!(!copy_item_disabled(&items));
    });
}

#[test]
fn selection_evicted_from_the_retained_buffer_fails_and_disables_copy() {
    let dir = TestConfigDir::new("nyaterm-selection-evicted");
    let mut cx = TestAppContext::single();
    let _cleanup = TestAppCleanup::new(&cx);
    let app = app_with_visible_local_session(&mut cx, dir.path(), "s1");
    cx.update_entity(&app, |app, cx| {
        let snapshot = seed_full_scrollback_session(app);
        let selection = TerminalSelection::from_range(
            TerminalBufferCellPos::new(0, 0),
            TerminalBufferCellPos::new(1, 8),
        );
        let view = app.terminal.view.views.get_mut("s1").unwrap();
        view.frame_snapshot = Some(snapshot.clone());
        app.terminal.selection.session_id = Some("s1".into());
        app.terminal.selection.selection = Some(selection);
        app.terminal.selection.source_history = Some(snapshot.history);

        push_lines_and_apply(app, cx, 30);

        assert!(matches!(
            app.terminal.selection.text.status,
            TerminalSelectionTextStatus::Failed(TerminalSelectionTextError::StaleHistory)
        ));
        assert_eq!(app.terminal.selection.selection, Some(selection));
        assert!(app.selected_terminal_text().is_none());
        let items = app.terminal_context_menu_items_for_session("s1".into(), cx);
        assert!(copy_item_disabled(&items), "Copy must be disabled");
    });
}

#[test]
fn eviction_during_a_pending_copy_still_delivers_the_text() {
    let dir = TestConfigDir::new("nyaterm-selection-pending-eviction");
    let mut cx = TestAppContext::single();
    let _cleanup = TestAppCleanup::new(&cx);
    let app = app_with_visible_local_session(&mut cx, dir.path(), "s1");
    let generation = cx.update_entity(&app, |app, cx| {
        let snapshot = seed_filled_scrollback_session(app, 624, 600);
        let selection = TerminalSelection::from_range(
            TerminalBufferCellPos::new(30, 0),
            TerminalBufferCellPos::new(31, 8),
        );
        let view = app.terminal.view.views.get_mut("s1").unwrap();
        view.frame_snapshot = Some(snapshot.clone());
        app.terminal.selection.session_id = Some("s1".into());
        app.terminal.selection.selection = Some(selection);
        app.terminal.selection.source_history = Some(snapshot.history);
        assert!(
            app.selected_terminal_text().is_none(),
            "the frame snapshot window must not cover the selection"
        );
        app.ensure_terminal_selection_text(cx);
        assert!(matches!(
            app.terminal.selection.text.status,
            TerminalSelectionTextStatus::Pending
        ));
        let generation = app.terminal.selection.text.generation;
        app.copy_terminal_selection(cx);
        assert_eq!(app.terminal.selection.text.pending_copy, Some(false));

        push_lines_and_apply(app, cx, 8);

        assert_eq!(app.terminal.selection.text.generation, generation);
        let reanchored = app
            .terminal
            .selection
            .selection
            .expect("selection survives");
        assert_eq!(reanchored.anchor.position.line, 22);
        assert_eq!(
            app.terminal.selection.text.key.as_ref().map(|key| key.1),
            Some(reanchored)
        );
        generation
    });
    cx.run_until_parked();
    cx.update_entity(&app, |app, cx| {
        let expected = "line 0030\nline 0031";
        assert_eq!(app.terminal.selection.text.generation, generation);
        assert!(matches!(
            app.terminal.selection.text.status,
            TerminalSelectionTextStatus::Ready(Some(_))
        ));
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some(expected)
        );
        assert_eq!(app.selected_terminal_text().as_deref(), Some(expected));
        assert_eq!(
            app.terminal
                .selection
                .selection
                .expect("selection survives")
                .anchor
                .position
                .line,
            22
        );
    });
}

#[test]
fn pending_menu_keeps_copy_enabled_and_copies_the_complete_selection_once() {
    let dir = TestConfigDir::new("nyaterm-cross-screen-copy");
    let mut cx = TestAppContext::single();
    let _cleanup = TestAppCleanup::new(&cx);
    let app = app_with_visible_local_session(&mut cx, dir.path(), "s1");
    let (key, generation) = cx.update_entity(&app, |app, cx| {
        seed_cross_screen_selection(app);
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        let items = app.terminal_context_menu_items_for_session("s1".into(), cx);
        let disabled = |label| {
            items
                .iter()
                .find(|item| item.test_label() == label)
                .unwrap()
                .test_presentation()
                .3
        };
        assert!(!disabled("Copy"));
        assert!(disabled("Find..."));
        assert!(disabled("Save as Quick Command"));
        assert!(!disabled("Paste"));
        assert!(matches!(
            app.terminal.selection.text.status,
            TerminalSelectionTextStatus::Pending
        ));
        let key = app.terminal.selection.text.key.clone().unwrap();
        let generation = app.terminal.selection.text.generation;
        // Automatic copy and repeated explicit copies coalesce on the same reply.
        app.copy_terminal_selection(cx);
        app.copy_terminal_selection_request(&key, generation, cx);
        app.copy_terminal_selection_or_visible(cx);
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("unchanged")
        );
        app.terminal.view.frame_pipeline.flush_for_test();
        (key, generation)
    });
    cx.run_until_parked();
    cx.update_entity(&app, |app, cx| {
        let expected = (0..500)
            .map(|i| format!("line {i:04}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            app.selected_terminal_text().as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some(expected.as_str())
        );
        let items = app.terminal_context_menu_items_for_session("s1".into(), cx);
        assert!(
            !items
                .iter()
                .find(|item| item.test_label() == "Find...")
                .unwrap()
                .test_presentation()
                .3
        );
        let other = app.terminal_context_menu_items_for_session("other".into(), cx);
        assert!(!other.iter().any(|item| item.test_label() == "Copy"));
        cx.write_to_clipboard(ClipboardItem::new_string("new clipboard".into()));
        app.copy_terminal_selection_request(&key, generation, cx);
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("new clipboard")
        );
    });
}

#[test]
fn clearing_replacing_or_switching_selection_discards_a_pending_copy() {
    for action in 0..4 {
        let dir = TestConfigDir::new("nyaterm-cross-screen-cancel");
        let mut cx = TestAppContext::single();
        let _cleanup = TestAppCleanup::new(&cx);
        let app = app_with_visible_local_session(&mut cx, dir.path(), "s1");
        cx.update_entity(&app, |app, cx| {
            seed_cross_screen_selection(app);
            cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
            app.copy_terminal_selection_or_visible(cx);
            app.terminal.view.frame_pipeline.flush_for_test();
            match action {
                0 => app.clear_terminal_selection(cx),
                1 => {
                    // The reply is already queued, but belongs to the old endpoints.
                    app.terminal
                        .selection
                        .selection
                        .as_mut()
                        .unwrap()
                        .head
                        .position
                        .line = 400;
                    app.ensure_terminal_selection_text(cx);
                    app.terminal.view.frame_pipeline.flush_for_test();
                }
                2 => app.clear_terminal_selection_state_for_session("s1"),
                _ => {
                    app.terminal.clear_activation_interaction();
                }
            }
        });
        cx.run_until_parked();
        cx.update_entity(&app, |_, cx| {
            assert_eq!(
                cx.read_from_clipboard()
                    .and_then(|item| item.text())
                    .as_deref(),
                Some("unchanged")
            );
        });
    }
}

#[test]
fn window_transfer_cancels_the_source_copy_and_reextracts_in_the_destination() {
    let source_dir = TestConfigDir::new("nyaterm-selection-transfer-source");
    let target_dir = TestConfigDir::new("nyaterm-selection-transfer-target");
    let mut cx = TestAppContext::single();
    let _cleanup = TestAppCleanup::new(&cx);
    let source = app_with_visible_local_session(&mut cx, source_dir.path(), "s1");
    let target = app_with_visible_local_session(&mut cx, target_dir.path(), "s1");
    let bundle = cx.update_entity(&source, |app, cx| {
        seed_cross_screen_selection(app);
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        app.copy_terminal_selection_or_visible(cx);
        app.terminal.view.frame_pipeline.flush_for_test();
        let frames = app
            .terminal
            .prepare_sessions_for_transfer(&["s1".into()])
            .unwrap();
        app.terminal.detach_sessions_for_transfer(frames)
    });
    cx.update_entity(&target, |app, cx| {
        assert!(app.terminal.attach_sessions_from_transfer(bundle).is_ok());
        app.ensure_terminal_selection_text(cx);
        app.terminal.view.frame_pipeline.flush_for_test();
    });
    cx.run_until_parked();
    cx.update_entity(&source, |app, cx| {
        assert!(app.terminal.selection.selection.is_none());
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("unchanged")
        );
    });
    cx.update_entity(&target, |app, _| {
        let expected = (0..500)
            .map(|i| format!("line {i:04}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            app.selected_terminal_text().as_deref(),
            Some(expected.as_str())
        );
    });
}

#[test]
fn stale_history_keeps_the_selection_menu_and_never_copies_visible_text() {
    let dir = TestConfigDir::new("nyaterm-cross-screen-stale");
    let mut cx = TestAppContext::single();
    let _cleanup = TestAppCleanup::new(&cx);
    let app = app_with_visible_local_session(&mut cx, dir.path(), "s1");
    cx.update_entity(&app, |app, cx| {
        seed_cross_screen_selection(app);
        app.terminal
            .view
            .frame_pipeline
            .submit_decoded_output("s1".into(), "\x1bcnew screen".into());
        app.terminal.view.frame_pipeline.flush_for_test();
        cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
        app.copy_terminal_selection_or_visible(cx);
        app.terminal.view.frame_pipeline.flush_for_test();
    });
    cx.run_until_parked();
    cx.update_entity(&app, |app, cx| {
        assert!(matches!(
            app.terminal.selection.text.status,
            TerminalSelectionTextStatus::Failed(TerminalSelectionTextError::StaleHistory)
        ));
        assert!(app.selected_terminal_text().is_none());
        let items = app.terminal_context_menu_items_for_session("s1".into(), cx);
        assert!(
            items
                .iter()
                .find(|item| item.test_label() == "Copy")
                .unwrap()
                .test_presentation()
                .3
        );
        app.copy_terminal_selection_or_visible(cx);
        assert_eq!(
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .as_deref(),
            Some("unchanged")
        );
    });
}
