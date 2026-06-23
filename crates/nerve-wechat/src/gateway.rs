//! The iLink Bot gateway client.
//!
//! [`WeixinGateway`] is the seam the bridge talks to (long-poll inbox + send),
//! so the bridge is unit-testable with a fake. [`IlinkGateway`] is the real
//! pure-Rust HTTP client against Tencent's gateway, grounded in the
//! `openclaw-weixin` source: `POST {base_url}/ilink/bot/{method}` with
//! `Authorization: Bearer <bot_token>`, `X-WECHAT-UIN` and `iLink-App-Id: bot`.

use crate::error::{WeixinError, WeixinResult};
use crate::http;
use crate::login::WeixinSession;
use crate::types::{BaseInfo, GetUpdatesReq, GetUpdatesResp, MessageItem, SendMessageReq};
use base64::Engine as _;
use std::time::Duration;

/// Login bootstrap host (`DEFAULT_BASE_URL` in the plugin). After login the gateway
/// returns a per-account `baseurl` to use for message calls.
pub const DEFAULT_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
/// CDN host for encrypted media (`CDN_BASE_URL`).
pub const CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";

/// Long-poll request timeout (the gateway holds `getupdates` up to ~35s).
const GET_UPDATES_TIMEOUT: Duration = Duration::from_secs(40);

/// The inbox + send seam the bridge depends on. Implemented by [`IlinkGateway`]
/// in production and by a fake in tests.
pub trait WeixinGateway {
    /// Long-poll for new messages from `cursor` (the `get_updates_buf`). Returns
    /// the response carrying any new messages and the advanced cursor.
    fn get_updates(&self, cursor: &str) -> WeixinResult<GetUpdatesResp>;

    /// Send a text reply to `to_user_id` within `session_id`.
    fn send_text(&self, to_user_id: &str, session_id: &str, text: &str) -> WeixinResult<()>;
}

/// The real pure-Rust iLink gateway client for one logged-in account.
pub struct IlinkGateway {
    agent: ureq::Agent,
    base_url: String,
    bot_token: String,
    /// Our own user id — the `from_user_id` on outbound messages.
    bot_user_id: String,
    uin_header: String,
    base_info: BaseInfo,
}

impl IlinkGateway {
    /// Build a gateway client from a logged-in [`WeixinSession`].
    #[must_use]
    pub fn new(session: WeixinSession) -> Self {
        Self {
            agent: http::agent(GET_UPDATES_TIMEOUT),
            base_url: session.base_url.trim_end_matches('/').to_string(),
            bot_token: session.bot_token,
            bot_user_id: session.user_id,
            uin_header: random_uin_header(),
            base_info: BaseInfo {
                channel_version: None,
                bot_agent: Some(format!("nerve-wechat/{}", env!("CARGO_PKG_VERSION"))),
            },
        }
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", self.bot_token),
            ),
            ("X-WECHAT-UIN".to_string(), self.uin_header.clone()),
            ("iLink-App-Id".to_string(), "bot".to_string()),
        ]
    }

    fn endpoint(&self, method: &str) -> String {
        format!("{}/ilink/bot/{method}", self.base_url)
    }
}

impl WeixinGateway for IlinkGateway {
    fn get_updates(&self, cursor: &str) -> WeixinResult<GetUpdatesResp> {
        let body = GetUpdatesReq {
            get_updates_buf: cursor.to_string(),
            base_info: self.base_info.clone(),
        };
        let value = http::post_json(
            &self.agent,
            &self.endpoint("getupdates"),
            &self.headers(),
            &body,
        )?;
        let resp: GetUpdatesResp =
            serde_json::from_value(value).map_err(|err| WeixinError::Parse(err.to_string()))?;
        check_ret(resp.ret)?;
        Ok(resp)
    }

    fn send_text(&self, to_user_id: &str, session_id: &str, text: &str) -> WeixinResult<()> {
        let body = SendMessageReq {
            from_user_id: self.bot_user_id.clone(),
            to_user_id: to_user_id.to_string(),
            session_id: session_id.to_string(),
            item_list: vec![MessageItem::text(text)],
            base_info: self.base_info.clone(),
        };
        let value = http::post_json(
            &self.agent,
            &self.endpoint("sendmessage"),
            &self.headers(),
            &body,
        )?;
        check_ret(
            value
                .get("ret")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0) as i32,
        )
    }
}

/// Map a non-zero gateway `ret` to an error (0 is success; long-poll timeouts
/// still return 0 with empty `msgs`).
fn check_ret(ret: i32) -> WeixinResult<()> {
    if ret == 0 {
        Ok(())
    } else {
        Err(WeixinError::Gateway { ret })
    }
}

/// `X-WECHAT-UIN`: base64 of a random-ish uint32 (nanosecond-derived; this header
/// is per-connection observability, not a secret).
fn random_uin_header() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    base64::engine::general_purpose::STANDARD.encode(nanos.to_be_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_joins_base_and_method_without_double_slash() {
        let gw = IlinkGateway::new(WeixinSession {
            bot_token: "t".into(),
            base_url: "https://host.example/".into(),
            account_id: "a@im.bot".into(),
            user_id: "bot_self".into(),
        });
        assert_eq!(
            gw.endpoint("getupdates"),
            "https://host.example/ilink/bot/getupdates"
        );
    }

    #[test]
    fn headers_carry_bearer_and_app_id() {
        let gw = IlinkGateway::new(WeixinSession {
            bot_token: "tok".into(),
            base_url: "https://host.example".into(),
            account_id: "a".into(),
            user_id: "bot_self".into(),
        });
        let headers = gw.headers();
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer tok")
        );
        assert!(
            headers
                .iter()
                .any(|(k, v)| k == "iLink-App-Id" && v == "bot")
        );
        assert!(headers.iter().any(|(k, _)| k == "X-WECHAT-UIN"));
    }

    #[test]
    fn check_ret_maps_nonzero_to_gateway_error() {
        assert!(check_ret(0).is_ok());
        assert!(matches!(
            check_ret(-14),
            Err(WeixinError::Gateway { ret: -14 })
        ));
    }
}
