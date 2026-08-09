//! Testable application orchestration, isolated from the desktop toolkit.

mod controller;
mod state;
mod update;
mod workspace;

pub use controller::Controller;
pub use state::{AppState, Question, QuestionRequest, Toast};
