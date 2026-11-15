//! The 212 terminal UI.
//!
//! Event-driven: the screen is redrawn only when input arrives, the runtime
//! sends an event, or (≈12 FPS) while an animation is visible. When idle the
//! loop sleeps on its inputs and uses no CPU.

pub mod anim;
pub mod app;
pub mod art;
pub mod commands;
pub mod glyphs;
pub mod input;
pub mod markdown;
pub mod theme;
pub mod view;

use std::time::{Duration, Instant};

use common::proto::{Event, Request};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as CEvent, EventStream, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use futures::StreamExt;
use tokio::sync::mpsc;

pub use app::{App, TuiOptions};

/// Run the interactive UI until the user quits and the runtime confirms shutdown.
pub async fn run(mut opts: TuiOptions, requests: mpsc::Sender<Request>, mut events: mpsc::Receiver<Event>) -> std::io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let mut out = std::io::stdout();
    let _ = execute!(out, EnableBracketedPaste);
    let enhanced = matches!(crossterm::terminal::supports_keyboard_enhancement(), Ok(true));
    if enhanced {
        let _ = execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES));
    }
    opts.keyboard_enhanced = enhanced;
    let result = event_loop(&mut terminal, App::new(opts), &requests, &mut events).await;
    if enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableBracketedPaste);
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    mut app: App,
    requests: &mpsc::Sender<Request>,
    events: &mut mpsc::Receiver<Event>,
) -> std::io::Result<()> {
    let mut input = EventStream::new();
    let mut tick = tokio::time::interval(anim::FRAME);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut quit_deadline: Option<Instant> = None;
    loop {
        for r in app.outbox.drain(..) {
            if requests.send(r).await.is_err() {
                app.exit = true;
            }
        }
        if app.exit {
            break;
        }
        if app.quitting {
            let d = *quit_deadline.get_or_insert_with(|| Instant::now() + Duration::from_secs(6));
            if Instant::now() > d {
                tracing::warn!("runtime did not confirm shutdown in time");
                break;
            }
        }
        if app.dirty {
            terminal.draw(|f| view::draw(f, &mut app))?;
            app.dirty = false;
        }
        let animating = app.animating() || app.quitting;
        tokio::select! {
            ev = input.next() => match ev {
                Some(Ok(CEvent::Key(k))) => app.on_key(k),
                Some(Ok(CEvent::Paste(s))) => app.on_paste(&s),
                Some(Ok(CEvent::Resize(..))) => app.dirty = true,
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    tracing::error!("terminal input error: {e}");
                    app.on_key(crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('c'), crossterm::event::KeyModifiers::CONTROL));
                }
                None => break,
            },
            ev = events.recv() => match ev {
                Some(e) => {
                    app.on_core(e);
                    // Coalesce bursts (token streams) into a single redraw.
                    while let Ok(e) = events.try_recv() {
                        app.on_core(e);
                    }
                }
                None => break,
            },
            _ = tick.tick(), if animating => app.tick(),
        }
    }
    Ok(())
}
