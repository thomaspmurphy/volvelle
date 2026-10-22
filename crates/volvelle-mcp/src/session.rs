//! Runtime monitor for the view lifecycle session type:
//!
//! ```text
//! View = !Render . (!Render)* . ?Event . end
//! ```
//!
//! The agent renders a view, may re-render it any number of times (each
//! re-render keeps what the user entered), then waits for exactly one answer.
//! LLMs cannot be type-checked, so every tool call is checked here instead,
//! and a call out of order is refused with the moves that are allowed next.

use std::collections::HashMap;

use serde_json::Value;
use volvelle_schema::Event;

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Rendered with fields or buttons; the human owes an answer.
    Awaiting,
    /// Rendered with nothing to answer: a status or progress display.
    Display,
    /// The human answered. Only a new render reopens the view.
    Answered(Value),
    /// Another view took over the pane.
    Replaced(String),
    /// The pane was closed.
    Closed,
}

#[derive(Default)]
pub struct Monitor {
    views: HashMap<String, State>,
    current: Option<String>,
}

pub struct Refusal(pub String);

impl Monitor {
    pub fn rendered(&mut self, view: &str, interactive: bool) {
        if let Some(prev) = self.current.replace(view.to_string())
            && prev != view
        {
            self.views.insert(prev, State::Replaced(view.to_string()));
        }
        let state = if interactive {
            State::Awaiting
        } else {
            State::Display
        };
        self.views.insert(view.to_string(), state);
    }

    pub fn closed(&mut self) {
        if let Some(view) = self.current.take() {
            self.views.insert(view, State::Closed);
        }
    }

    /// May the agent wait on this view?
    pub fn check_wait(&self, view: &str) -> Result<(), Refusal> {
        let refuse = |why: String, next: &str| Err(Refusal(format!("{why}; allowed next: {next}")));
        match self.views.get(view) {
            Some(State::Awaiting) => Ok(()),
            None => refuse(format!("no view {view:?} has been rendered"), "render"),
            Some(State::Display) => refuse(
                format!("view {view:?} has no fields or buttons, so nothing will answer it"),
                "render (add a button or field), close",
            ),
            Some(State::Answered(answer)) => refuse(
                format!("view {view:?} was already answered with {answer}"),
                "render",
            ),
            Some(State::Replaced(by)) => refuse(
                format!("view {view:?} was replaced by {by:?}"),
                "wait_event on that view, render",
            ),
            Some(State::Closed) => refuse(format!("view {view:?} was closed"), "render"),
        }
    }

    /// Feeds a viewer event in. Returns it if it answers `view`.
    pub fn accept(&mut self, view: &str, event: &Event) -> Option<Value> {
        let answered = match event {
            Event::Submit { view, .. }
            | Event::Select { view, .. }
            | Event::Answer { view, .. }
            | Event::Cancel { view, .. } => view,
            Event::Closed => {
                self.closed();
                return None;
            }
        };
        if answered != view || self.views.get(view) != Some(&State::Awaiting) {
            return None;
        }
        let value = serde_json::to_value(event).expect("event serialises");
        self.views
            .insert(view.to_string(), State::Answered(value.clone()));
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn submit(view: &str) -> Event {
        Event::Submit {
            view: view.into(),
            button: "ok".into(),
            values: Map::new(),
        }
    }

    #[test]
    fn one_answer_per_render() {
        let mut m = Monitor::default();
        assert!(m.check_wait("a").is_err());
        m.rendered("a", true);
        m.rendered("a", true);
        assert!(m.check_wait("a").is_ok());
        assert!(m.accept("a", &submit("a")).is_some());
        let Err(Refusal(msg)) = m.check_wait("a") else {
            panic!()
        };
        assert!(msg.ends_with("allowed next: render"));
        m.rendered("a", true);
        assert!(m.check_wait("a").is_ok());
    }

    #[test]
    fn display_views_and_replacement_refuse_waits() {
        let mut m = Monitor::default();
        m.rendered("status", false);
        assert!(m.check_wait("status").is_err());
        m.rendered("form", true);
        let Err(Refusal(msg)) = m.check_wait("status") else {
            panic!()
        };
        assert!(msg.contains("replaced by \"form\""));
        assert!(m.accept("form", &submit("status")).is_none());
        m.accept("form", &Event::Closed);
        assert!(m.check_wait("form").is_err());
    }
}
