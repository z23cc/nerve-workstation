//! iLink Bot gateway wire types (personal WeChat / 微信).
//!
//! Field and endpoint shapes are taken from Tencent's `openclaw-weixin` channel
//! plugin source (`src/api/types.ts`). Inbound types default every field so a
//! partial/extended envelope still deserializes; outbound types serialize only
//! what the gateway needs.

use serde::{Deserialize, Serialize};

/// `MessageItemType` — the item payload discriminant. Best-effort integer values
/// mirroring the plugin's enum; the bridge does not hard-depend on these (it keys
/// on the presence of text and dedups by `message_id`), so a wrong guess degrades
/// gracefully rather than misbehaving.
pub mod item_type {
    pub const NONE: i32 = 0;
    pub const TEXT: i32 = 1;
    pub const IMAGE: i32 = 2;
    pub const VOICE: i32 = 3;
    pub const FILE: i32 = 4;
    pub const VIDEO: i32 = 5;
}

/// `BaseInfo` attached to every request (`channel_version`, `bot_agent`). Both are
/// observability-only and optional.
#[derive(Debug, Clone, Default, Serialize)]
pub struct BaseInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot_agent: Option<String>,
}

/// A text payload (`TextItem`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct TextItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// One item in a message's `item_list`. Only TEXT is modeled in full; other types
/// round-trip via their discriminant so inbound media messages still deserialize.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MessageItem {
    #[serde(rename = "type", default)]
    pub item_type: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_item: Option<TextItem>,
}

impl MessageItem {
    /// Build an outbound TEXT item.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            item_type: item_type::TEXT,
            text_item: Some(TextItem {
                text: Some(text.into()),
            }),
        }
    }

    /// The item's text, if it is a TEXT item carrying non-empty content.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        self.text_item
            .as_ref()
            .and_then(|t| t.text.as_deref())
            .filter(|s| !s.is_empty())
    }
}

/// An inbound message from `getupdates` (`WeixinMessage`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct WeixinMessage {
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub from_user_id: String,
    #[serde(default)]
    pub to_user_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub group_id: String,
    #[serde(default)]
    pub create_time_ms: i64,
    #[serde(default)]
    pub message_type: i32,
    #[serde(default)]
    pub message_state: i32,
    #[serde(default)]
    pub item_list: Vec<MessageItem>,
    #[serde(default)]
    pub context_token: String,
}

impl WeixinMessage {
    /// Concatenated text of all TEXT items, or `None` when the message carries no
    /// text (e.g. a media-only message).
    #[must_use]
    pub fn text(&self) -> Option<String> {
        let joined: String = self
            .item_list
            .iter()
            .filter_map(MessageItem::as_text)
            .collect::<Vec<_>>()
            .join("");
        (!joined.is_empty()).then_some(joined)
    }

    /// Whether this message is from a group chat (`group_id` set) vs a direct chat.
    #[must_use]
    pub fn is_group(&self) -> bool {
        !self.group_id.is_empty()
    }
}

/// `getupdates` request body: the long-poll cursor plus base info.
#[derive(Debug, Clone, Serialize)]
pub struct GetUpdatesReq {
    pub get_updates_buf: String,
    pub base_info: BaseInfo,
}

/// `getupdates` response: a return code, any new messages, and the advanced cursor.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GetUpdatesResp {
    #[serde(default)]
    pub ret: i32,
    #[serde(default)]
    pub msgs: Vec<WeixinMessage>,
    #[serde(default)]
    pub get_updates_buf: String,
}

/// `sendmessage` request body.
#[derive(Debug, Clone, Serialize)]
pub struct SendMessageReq {
    pub from_user_id: String,
    pub to_user_id: String,
    pub session_id: String,
    pub item_list: Vec<MessageItem>,
    pub base_info: BaseInfo,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbound_message_deserializes_partial_envelope_and_extracts_text() {
        // A realistic-ish getupdates payload with extra/missing fields.
        let raw = r#"{
            "ret": 0,
            "get_updates_buf": "cursor-2",
            "msgs": [{
                "message_id": "m1",
                "from_user_id": "u_alice",
                "session_id": "s1",
                "item_list": [
                    { "type": 1, "text_item": { "text": "hello " } },
                    { "type": 1, "text_item": { "text": "world" } }
                ]
            }]
        }"#;
        let resp: GetUpdatesResp = serde_json::from_str(raw).expect("parse");
        assert_eq!(resp.get_updates_buf, "cursor-2");
        assert_eq!(resp.msgs.len(), 1);
        let msg = &resp.msgs[0];
        assert_eq!(msg.from_user_id, "u_alice");
        assert!(!msg.is_group());
        assert_eq!(msg.text().as_deref(), Some("hello world"));
    }

    #[test]
    fn media_only_message_has_no_text() {
        let msg = WeixinMessage {
            item_list: vec![MessageItem {
                item_type: item_type::IMAGE,
                text_item: None,
            }],
            ..Default::default()
        };
        assert_eq!(msg.text(), None);
    }

    #[test]
    fn outbound_text_item_serializes_with_type_tag() {
        let item = MessageItem::text("hi");
        let value = serde_json::to_value(&item).expect("serialize");
        assert_eq!(value["type"], item_type::TEXT);
        assert_eq!(value["text_item"]["text"], "hi");
    }

    #[test]
    fn base_info_omits_unset_fields() {
        let value = serde_json::to_value(BaseInfo::default()).expect("serialize");
        assert!(value.get("channel_version").is_none());
        assert!(value.get("bot_agent").is_none());
    }
}
