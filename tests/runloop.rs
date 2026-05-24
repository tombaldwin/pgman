//! End-to-end tests of the actual `App::run_with` async loop. This
//! goes one level deeper than `tests/journeys.rs`, which drives
//! `on_key` directly — here we drive the full select! loop, frame
//! ticks and all, via the new `HeadlessTui` + a synthetic
//! `UnboundedReceiver<Event>`.
//!
//! Each test pushes a finite sequence of events into the channel
//! and then drops the sender. The loop terminates cleanly when the
//! event channel closes (treated as "operator quit"). A
//! `tokio::time::timeout` wrapper catches any test that would
//! otherwise hang.

use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use pgman::app::{App, ConnState, Mode};
use pgman::safety::SafetyConfig;
use pgman::theme::Theme;
use pgman::tui::HeadlessTui;

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn ctrl_key(c: char) -> Event {
    Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

fn settled_app() -> App {
    let mut a = App::new(Theme::default(), None, Vec::new(), SafetyConfig::default());
    a.splash_visible = false;
    a.splash_until = None;
    a.conn_state = ConnState::Connected {
        server_version: "16.0".into(),
    };
    a
}

#[tokio::test]
async fn run_loop_typing_and_quit_terminates() {
    let mut app = settled_app();
    app.mode = Mode::Editor;
    let mut tui = HeadlessTui::default();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();

    // Type "hi\n", then Esc to return to Normal, then `q` to quit.
    for code in ["h", "i"].iter() {
        tx.send(key(KeyCode::Char(code.chars().next().unwrap()))).unwrap();
    }
    tx.send(key(KeyCode::Esc)).unwrap();
    tx.send(key(KeyCode::Char('q'))).unwrap();
    drop(tx); // closes the channel — defensive, in case `q` doesn't trip should_quit

    tokio::time::timeout(Duration::from_secs(2), app.run_with(&mut tui, rx))
        .await
        .expect("loop should terminate quickly")
        .expect("run_with returned Err");

    assert!(app.should_quit);
    assert_eq!(app.editor_buffer, "hi");
    // Drew at least the initial frame + after each event.
    assert!(tui.draws >= 1, "HeadlessTui should have rendered at least once");
}

#[tokio::test]
async fn run_loop_dropped_event_channel_terminates_loop() {
    // Contract: if the event producer is gone, the loop should quit
    // cleanly. This is how the production code shuts down on
    // crossterm disconnect, and it's how our tests do bounded runs.
    let mut app = settled_app();
    app.mode = Mode::Normal;
    let mut tui = HeadlessTui::default();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    drop(tx); // no events ever sent

    let _ = tokio::time::timeout(Duration::from_secs(2), app.run_with(&mut tui, rx))
        .await
        .expect("loop should terminate when channel closes");
    assert!(app.should_quit);
}

#[tokio::test]
async fn run_loop_ctrl_c_in_normal_mode_quits() {
    let mut app = settled_app();
    app.mode = Mode::Normal;
    let mut tui = HeadlessTui::default();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    tx.send(ctrl_key('c')).unwrap();
    drop(tx);

    tokio::time::timeout(Duration::from_secs(2), app.run_with(&mut tui, rx))
        .await
        .expect("loop should terminate quickly")
        .unwrap();
    assert!(app.should_quit);
}

#[tokio::test]
async fn run_loop_external_edit_flag_triggers_suspend_resume() {
    // `\e` (Ctrl-X in editor mode) sets external_edit_pending. The
    // loop should call tui.suspend / external editor / tui.resume.
    // Point `$EDITOR` at /usr/bin/true (or the equivalent) so the
    // subprocess exits without doing anything.
    std::env::set_var("EDITOR", "true");

    let mut app = settled_app();
    app.mode = Mode::Editor;
    let mut tui = HeadlessTui::default();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    tx.send(ctrl_key('x')).unwrap(); // \e
    // Followed by something to give the loop another iteration.
    tx.send(key(KeyCode::Char('a'))).unwrap();
    drop(tx);

    tokio::time::timeout(Duration::from_secs(5), app.run_with(&mut tui, rx))
        .await
        .expect("loop should terminate quickly")
        .unwrap();

    assert_eq!(tui.suspends, 1, "expected exactly one suspend");
    assert_eq!(tui.resumes, 1, "expected exactly one resume");
    // The `$EDITOR=true` subprocess exits 0 without modifying the
    // file → buffer becomes whatever was in it (empty here), and the
    // status reports loaded N char(s).
    assert!(
        app.last_status
            .as_deref()
            .unwrap_or("")
            .contains("loaded"),
        "status: {:?}",
        app.last_status
    );
}
