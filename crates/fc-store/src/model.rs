//! Types the store needs that are not part of the shared domain in `fc-core`.
//!
//! `fc-core::Conversation` is the read shape; it is not how a conversation is
//! created (no id yet) or listed (a list row needs aggregates no single
//! `Conversation` carries). Those shapes live here instead of growing
//! `fc-core` with persistence-flavoured fields every other crate would have to
//! carry around unused.

use fc_core::{AudioSource, ConversationId, EngineInfo, GroupId, TagId, UnixMillis};

/// Everything needed to start a conversation row. `status` is implied
/// (`Active`) and `ended_at` does not exist yet, which is why this is not
/// just `Conversation` minus its id.
#[derive(Debug, Clone, PartialEq)]
pub struct NewConversation {
    pub title: String,
    pub group: Option<GroupId>,
    pub started_at: UnixMillis,
    pub source: AudioSource,
    pub mic_track: bool,
    pub engine: EngineInfo,
    pub voxtype_meeting_id: Option<String>,
}

/// Criteria for `Store::list_conversations`. `None` on any field means "do not
/// filter on this".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConversationFilter {
    pub group: Option<GroupId>,
    pub tag: Option<TagId>,
    /// Plain-text match against the conversation title. Unlike `search`, this
    /// is a `LIKE` match on metadata, not an FTS lookup over segment text.
    /// Treated as literal text, not a pattern: `%` and `_` in here match
    /// themselves, not "anything"/"any one character" -- `list_conversations`
    /// escapes them before building the `LIKE` pattern.
    pub query: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// One row of a conversation list, for a caller that needs no more than a
/// label and a group.
///
/// Deliberately thin. It once carried a duration, a segment count and a
/// source label; nothing ever read them, and the segment count cost a
/// correlated `COUNT(*)` over every listed row. A caller that wants the whole
/// record — source, engine, status, the imported voxtype id — asks
/// `Store::list_conversations_full` for it instead of paying for aggregates
/// twice.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationSummary {
    pub id: ConversationId,
    pub title: String,
    pub started_at: UnixMillis,
    pub group_name: Option<String>,
    pub tags: Vec<fc_core::Tag>,
}

/// One match from `Store::search`, carrying enough of the surrounding
/// conversation to jump straight to it from a results list.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchHit {
    pub conversation_id: ConversationId,
    pub conversation_title: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Marked-up excerpt from `snippet()`: plain text with match spans
    /// wrapped in `\u{2039}...\u{203a}` for the UI to highlight.
    pub snippet: String,
}
