use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{Event, EventStream, KeyEvent, KeyEventKind, MouseEvent, MouseEventKind};
use futures::StreamExt;
use tokio::{select, sync::mpsc, time::Interval};

use crate::action::Action;

pub enum AppEvent {
    Tick,
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize,
    BackgroundAction(Action),
}

pub struct EventHandler {
    events: EventStream,
    tick: Interval,
    action_rx: mpsc::UnboundedReceiver<Action>,
}

impl EventHandler {
    pub fn new(tick_rate: Duration, action_rx: mpsc::UnboundedReceiver<Action>) -> Self {
        let mut tick = tokio::time::interval(tick_rate);
        // Skip the first immediate tick
        tick.reset();

        Self {
            events: EventStream::new(),
            tick,
            action_rx,
        }
    }

    pub async fn next(&mut self) -> Result<AppEvent> {
        // Loop so ignored terminal events (key releases, mouse motion
        // and drags — high-frequency under mouse capture) are dropped
        // here instead of waking the render loop for every one.
        loop {
            select! {
                Some(Ok(event)) = self.events.next() => {
                    match event {
                        Event::Key(key)
                            if key.kind == KeyEventKind::Press =>
                        {
                            return Ok(AppEvent::Key(key));
                        }
                        Event::Mouse(mouse) if is_actionable_mouse(mouse.kind) => {
                            return Ok(AppEvent::Mouse(mouse));
                        }
                        Event::Paste(text) => {
                            return Ok(AppEvent::Paste(text));
                        }
                        Event::Resize(_, _) => {
                            return Ok(AppEvent::Resize);
                        }
                        Event::Key(_) | Event::Mouse(_) | Event::FocusGained | Event::FocusLost => {}
                    }
                }
                Some(action) = self.action_rx.recv() => {
                    return Ok(AppEvent::BackgroundAction(action));
                }
                _ = self.tick.tick() => {
                    return Ok(AppEvent::Tick);
                }
            }
        }
    }
}

/// Mouse events the app reacts to; everything else (motion, drags,
/// button releases) is noise under mouse capture.
fn is_actionable_mouse(kind: MouseEventKind) -> bool {
    matches!(
        kind,
        MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    )
}
