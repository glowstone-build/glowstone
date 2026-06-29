//! Floating windows + app preferences. Each modeless `egui::Window` lives in its
//! own submodule so it can be designed and iterated on independently (rather than
//! crammed into one file): the Preferences dialog, the Fixture Profile editor,
//! the quick-select palette, and the About / Keyboard-Shortcuts boxes. Kept out
//! of `panels.rs` (the docked panels), which these float over.

mod about;
mod add_menu;
mod operator_search;
mod patch_dialog;
mod perf_overlay;
mod preferences;
mod profile_editor;
mod quick_select;
mod shortcuts;
mod unpatch_dialog;

pub use about::about_window;
pub use add_menu::{AddAction, AddMenuState, add_menu_window};
pub use operator_search::{OperatorSearchState, operator_search_window};
pub use patch_dialog::{PatchDialog, patch_dialog_window};
pub use perf_overlay::perf_overlay_window;
pub use preferences::{KeymapEditorState, LabelMode, Preferences, preferences_window};
pub use profile_editor::{ProfileEditor, profile_editor_window};
pub use quick_select::quick_select_window;
pub use shortcuts::shortcuts_window;
pub use unpatch_dialog::{UnpatchDialog, unpatch_dialog_window};
