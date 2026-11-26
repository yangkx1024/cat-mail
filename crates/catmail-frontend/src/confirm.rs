//! What the app asks before a change that can't be undone: deleting messages or a folder
//! for good, removing an account, sending again what may have arrived. Every view asks it
//! the same way.

/// What asking about a change that can't be undone says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub title: String,
    pub description: String,
    /// The button that goes on: "Delete", "Remove".
    pub action: &'static str,
}
