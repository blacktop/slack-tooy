use std::time::Duration;

use color_eyre::eyre::Result;
use crossterm::event::{Event, EventStream, KeyEvent, KeyEventKind};
use futures::StreamExt;
use tokio::{select, sync::mpsc, time::Interval};

use crate::action::Action;

pub enum AppEvent {
    Tick,
    Key(KeyEvent),
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
        select! {
            Some(Ok(event)) = self.events.next() => {
                match event {
                    Event::Key(key)
                        if key.kind == KeyEventKind::Press =>
                    {
                        Ok(AppEvent::Key(key))
                    }
                    Event::Resize(_, _) => {
                        Ok(AppEvent::Resize)
                    }
                    _ => Ok(AppEvent::Tick),
                }
            }
            Some(action) = self.action_rx.recv() => {
                Ok(AppEvent::BackgroundAction(action))
            }
            _ = self.tick.tick() => {
                Ok(AppEvent::Tick)
            }
        }
    }
}
