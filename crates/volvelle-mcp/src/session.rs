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
//!
//! A pattern can extend the type after its answer. confirm-destructive is
//! `!Spec . ?{confirm: !Outcome . end, cancel: end}`: once confirmed, the
//! only move left is to render the same view again with what happened.

use std::collections::HashMap;

use serde_json::Value;
use volvelle_schema::{Event, Obligation, PatternInfo};

#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// Rendered with fields or buttons; the human owes an answer.
    Awaiting,
    /// Rendered with nothing to answer: a status or progress display.
    Display,
    /// The human answered. Only a new render reopens the view.
    Answered(Value),
    /// A pattern answered and the agent owes an outcome. Only a render of
    /// this view discharges it.
    Owes(Value),
    /// Another view took over the pane.
    Replaced(String),
    /// The pane was closed.
    Closed,
}

#[derive(Default)]
pub struct Monitor {
    views: HashMap<String, State>,
    current: Option<String>,
    /// The pattern each view holds, if any.
    patterns: HashMap<String, &'static PatternInfo>,
}

pub struct Refusal(pub String);

impl Monitor {
    pub fn rendered(
        &mut self,
        view: &str,
        interactive: bool,
        pattern: Option<&'static PatternInfo>,
    ) {
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
        match pattern {
            Some(p) => self.patterns.insert(view.to_string(), p),
            None => self.patterns.remove(view),
        };
    }

    /// The pane closed. An owed outcome survives: rendering reopens the pane.
    pub fn closed(&mut self) {
        if let Some(view) = self.current.take()
            && !self.owes(&view)
        {
            self.views.insert(view, State::Closed);
        }
    }

    /// Whether the agent owes this view an outcome.
    pub fn owes(&self, view: &str) -> bool {
        matches!(self.views.get(view), Some(State::Owes(_)))
    }

    /// While an outcome is owed, every move but rendering that view is refused.
    fn owed(&self, view: Option<&str>) -> Result<(), Refusal> {
        match self.views.iter().find(|(_, s)| matches!(s, State::Owes(_))) {
            Some((owed, _)) if Some(owed.as_str()) != view => Err(owed_refusal(owed)),
            _ => Ok(()),
        }
    }

    pub fn check_render(&self, view: &str) -> Result<(), Refusal> {
        self.owed(Some(view))
    }

    pub fn check_close(&self) -> Result<(), Refusal> {
        self.owed(None)
    }

    /// May the agent wait on this view?
    pub fn check_wait(&self, view: &str) -> Result<(), Refusal> {
        self.owed(None)?;
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
            Some(State::Owes(_)) => Err(owed_refusal(view)),
        }
    }

    /// Feeds a viewer event in. Returns it if it answers `view`; a pattern
    /// answer gains a `next` hint saying what the agent owes.
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
            // Saving is the human keeping a view, not answering it, and an
            // update's acknowledgement answers nothing either.
            Event::Saved { .. } | Event::Updated { .. } => return None,
        };
        if answered != view || self.views.get(view) != Some(&State::Awaiting) {
            return None;
        }
        let mut value = serde_json::to_value(event).expect("event serialises");
        let mut state = State::Answered(value.clone());
        if let (Event::Answer { value: answer, .. }, Some(p)) = (event, self.patterns.get(view)) {
            let next = match (p.after_answer)(answer) {
                Obligation::None => "render or close".to_string(),
                Obligation::ReportOutcome => {
                    state = State::Owes(value.clone());
                    format!("render view {view:?} to report the outcome")
                }
            };
            value["next"] = next.into();
        }
        self.views.insert(view.to_string(), state);
        Some(value)
    }
}

fn owed_refusal(view: &str) -> Refusal {
    Refusal(format!(
        "view {view:?} was confirmed; report the outcome by rendering view {view:?}; \
         allowed next: render view {view:?}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, json};
    use volvelle_schema::library::Scope;
    use volvelle_schema::{CHOOSE_WITH_PREVIEW, CONFIRM_DESTRUCTIVE};

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
        m.rendered("a", true, None);
        m.rendered("a", true, None);
        assert!(m.check_wait("a").is_ok());
        assert!(m.accept("a", &submit("a")).is_some());
        let Err(Refusal(msg)) = m.check_wait("a") else {
            panic!()
        };
        assert!(msg.ends_with("allowed next: render"));
        m.rendered("a", true, None);
        assert!(m.check_wait("a").is_ok());
    }

    #[test]
    fn saving_neither_answers_nor_disturbs_a_wait() {
        let mut m = Monitor::default();
        m.rendered("a", true, None);
        let saved = Event::Saved {
            view: "a".into(),
            name: "sheet".into(),
            scope: Scope::User,
            path: "/lib/sheet.json".into(),
        };
        assert!(m.accept("a", &saved).is_none());
        assert!(m.check_wait("a").is_ok());
        assert!(m.accept("a", &submit("a")).is_some());
        assert!(m.accept("a", &saved).is_none());
        let Err(Refusal(msg)) = m.check_wait("a") else {
            panic!()
        };
        assert!(msg.contains("already answered"));
    }

    #[test]
    fn display_views_and_replacement_refuse_waits() {
        let mut m = Monitor::default();
        m.rendered("status", false, None);
        assert!(m.check_wait("status").is_err());
        m.rendered("form", true, None);
        let Err(Refusal(msg)) = m.check_wait("status") else {
            panic!()
        };
        assert!(msg.contains("replaced by \"form\""));
        assert!(m.accept("form", &submit("status")).is_none());
        m.accept("form", &Event::Closed);
        assert!(m.check_wait("form").is_err());
    }

    fn answer(view: &str, value: Value) -> Event {
        Event::Answer {
            view: view.into(),
            id: "p".into(),
            value,
            values: Map::new(),
        }
    }

    #[test]
    fn confirmed_view_owes_an_outcome() {
        let mut m = Monitor::default();
        m.rendered("drop", true, Some(&CONFIRM_DESTRUCTIVE));
        let got = m
            .accept("drop", &answer("drop", json!({"confirmed": true})))
            .unwrap();
        assert_eq!(got["next"], "render view \"drop\" to report the outcome");
        assert!(m.owes("drop"));
        let refusal = "view \"drop\" was confirmed; report the outcome by rendering view \"drop\"; \
                       allowed next: render view \"drop\"";
        for result in [
            m.check_render("other"),
            m.check_close(),
            m.check_wait("drop"),
        ] {
            let Err(Refusal(msg)) = result else { panic!() };
            assert_eq!(msg, refusal);
        }
        m.closed();
        assert!(m.owes("drop"), "closing the pane does not discharge it");
        assert!(m.check_render("drop").is_ok());
        m.rendered("drop", false, None);
        assert!(!m.owes("drop"));
        assert!(m.check_close().is_ok() && m.check_render("other").is_ok());
    }

    #[test]
    fn cancelled_confirm_and_choices_end_normally() {
        let mut m = Monitor::default();
        m.rendered("drop", true, Some(&CONFIRM_DESTRUCTIVE));
        let got = m
            .accept("drop", &answer("drop", json!({"confirmed": false})))
            .unwrap();
        assert_eq!(got["next"], "render or close");
        assert!(m.check_render("other").is_ok() && m.check_close().is_ok());

        m.rendered("pick", true, Some(&CHOOSE_WITH_PREVIEW));
        assert!(
            m.accept("pick", &answer("pick", json!({"selected": "a"})))
                .is_some()
        );
        assert!(m.check_close().is_ok());

        m.rendered("drop", true, Some(&CONFIRM_DESTRUCTIVE));
        let cancel = Event::Cancel {
            view: "drop".into(),
            values: Map::new(),
        };
        let got = m.accept("drop", &cancel).unwrap();
        assert!(got.get("next").is_none());
        assert!(m.check_close().is_ok());
    }
}
