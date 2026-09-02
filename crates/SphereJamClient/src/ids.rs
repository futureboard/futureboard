//! Typed identifiers.
//!
//! Every id in a jam is a string on the wire, and almost all of them are ULID
//! shaped, so `String` everywhere would let a stream id be passed where a
//! participant id is expected and compile. These newtypes cost nothing at
//! runtime and make that a type error.
//!
//! The distinction that actually matters musically is [`UserId`] versus
//! username: the id is immutable and is the only value anything routes on; a
//! username is a mutable display alias its owner can change and someone else
//! can later take. Nothing in this crate keys a map on a username.

use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! wire_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Wrap a value that came off the wire or out of a project file.
            pub fn new(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }

            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_string())
            }
        }
    };
}

wire_id! {
    /// A Futureboard account. Immutable, and the only identity anything routes
    /// on. One account may hold several participants in the same jam.
    UserId
}

wire_id! {
    /// One physical endpoint of one account: a laptop running Studio and a
    /// phone used as a talkback mic are two devices. Client generated and
    /// stable across restarts, so a reconnect re-attaches instead of leaving a
    /// ghost participant behind.
    DeviceId
}

wire_id! {
    /// One (account, device) attachment to one jam.
    ParticipantId
}

wire_id! {
    /// One publishable audio stream. Canonical for session routing.
    StreamId
}

wire_id! {
    /// One jam session.
    JamId
}

/// The compact numeric alias a stream carries in media packet headers.
///
/// It is a separate type from [`StreamId`] because the two live on different
/// planes: the control plane talks in ULIDs so logs stay readable, the media
/// plane pays four bytes per packet instead of thirty. The server sends the
/// mapping explicitly at publish time; it is never inferred from publish order,
/// which breaks the first time anybody unpublishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MediaAlias(pub u32);

impl fmt::Display for MediaAlias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Build the canonical, human-readable routing path for one stream channel.
///
/// Identifiers only. A UI renders `jam/@hachi224/Guitar : L R` from stream
/// metadata; this is what the wire and the logs agree on.
pub fn canonical_channel_path(
    user: &UserId,
    device: &DeviceId,
    stream: &StreamId,
    channel: usize,
) -> String {
    format!("jam/{user}/{device}/{stream}/ch/{channel}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_json_as_bare_strings() {
        let id = StreamId::new("str_01K4S8");
        let encoded = serde_json::to_string(&id).expect("stream ids serialize");
        assert_eq!(encoded, "\"str_01K4S8\"");
        let decoded: StreamId = serde_json::from_str(&encoded).expect("stream ids deserialize");
        assert_eq!(decoded, id);
    }

    #[test]
    fn canonical_paths_are_built_from_ids_not_usernames() {
        let path = canonical_channel_path(
            &UserId::new("usr_1"),
            &DeviceId::new("dev_2"),
            &StreamId::new("str_3"),
            1,
        );
        assert_eq!(path, "jam/usr_1/dev_2/str_3/ch/1");
    }
}
