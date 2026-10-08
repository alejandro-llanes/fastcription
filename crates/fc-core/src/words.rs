//! The registry of words and expressions a reader did not know.
//!
//! This app exists for someone who follows English better read than heard.
//! The transcript gets them through the meeting; the thing they are still
//! missing afterwards is the three or four expressions in it they had never
//! met. A registry is where those go, with the line they were said in, so the
//! meaning can be looked up in context rather than in the abstract — "table
//! this" means nothing until you know it was said about a decision.

use crate::{ConversationId, UnixMillis, WordId};

/// One entry: an expression, where it was heard, and what it turned out to
/// mean.
///
/// `meaning`, `translation` and `example` are what the meaning server
/// answered, and are editable afterwards: the server is a small model and its
/// translations of idioms are sometimes literal, which the English meaning
/// beside them is there to catch. All three are `None` until a lookup has
/// run, which is a state the registry shows rather than hides.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub id: WordId,
    /// The word or expression itself, as the reader selected it.
    pub expression: String,
    /// The transcript line it was taken from, which is what the lookup is
    /// asked to explain it in the light of.
    pub context: String,
    /// The conversation it was heard in, if that conversation still exists.
    pub conversation: Option<ConversationId>,
    /// Where in that conversation, so the registry can jump back to it.
    pub start_ms: Option<u64>,
    /// A short English meaning, in context.
    pub meaning: Option<String>,
    /// The expression in the reader's own language.
    pub translation: Option<String>,
    /// One English example sentence.
    pub example: Option<String>,
    pub created_at: UnixMillis,
}

impl Word {
    /// Whether a lookup has answered for this entry.
    pub fn looked_up(&self) -> bool {
        self.meaning.is_some() || self.translation.is_some()
    }
}
