//! Newtyped row identifiers.
//!
//! These exist so a group id cannot be passed where a conversation id belongs.
//! SQLite hands out `i64` row ids, so that is what they wrap.

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! row_id {
    ($name:ident, $what:literal) => {
        #[doc = concat!("Row id of a ", $what, ".")]
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl $name {
            pub fn get(self) -> i64 {
                self.0
            }
        }

        impl From<i64> for $name {
            fn from(v: i64) -> Self {
                Self(v)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

row_id!(ConversationId, "conversation");
row_id!(GroupId, "group");
row_id!(TagId, "tag");
