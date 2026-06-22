//! Floating windows + app preferences. Each modeless `egui::Window` lives in its
//! own submodule so it can be designed and iterated on independently (rather than
//! crammed into one file): the Preferences dialog, the Fixture Profile editor,
//! the quick-select palette, and the About / Keyboard-Shortcuts boxes. Kept out
//! of `panels.rs` (the docked panels), which these float over.

mod about;
mod preferences;
mod profile_editor;
mod quick_select;
mod shortcuts;

pub use about::about_window;
pub use preferences::{preferences_window, LabelMode, Preferences};
pub use profile_editor::{profile_editor_window, ProfileEditor};
pub use quick_select::quick_select_window;
pub use shortcuts::shortcuts_window;
