//! IPC 線上協定 (SDD §4.3)。
//!
//! NDJSON: 每行一個 JSON 物件。對端是 voicetype-fcitx5 的手寫解析器,
//! 因此這裡的欄位名與型別必須逐一對齊 —— 對端會忽略未知欄位, 但型別
//! 不符的欄位會被靜默丟棄, 不會報錯。

use serde::{Deserialize, Serialize};

/// Addon → Daemon
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Start {
        session: u64,
        /// `InputContext::program()`。部分應用程式回空字串 (SDD 未解問題 #2),
        /// 此時退回 default profile。
        #[serde(default)]
        program: String,
        /// 縱深防禦: addon 已在密碼欄位拒絕啟動錄音 (SDD §7),
        /// daemon 收到 true 仍會拒絕。
        #[serde(default)]
        is_password: bool,
    },
    Stop {
        session: u64,
    },
    Cancel {
        session: u64,
    },
    /// 降級鏈② (SDD §4.8): addon 的目標 InputContext 已消失。
    FallbackClipboard {
        text: String,
    },
    Ping,
}

/// Daemon → Addon
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Result {
        session: u64,
        text: String,
    },
    Error {
        #[serde(skip_serializing_if = "Option::is_none")]
        session: Option<u64>,
        code: ErrorCode,
        text: String,
    },
    /// 選用的 UI 提示。M0 不送, 保留協定位置。
    State {
        session: u64,
        value: SessionState,
    },
    Pong,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NoAudioDevice,
    ModelLoadFailed,
    TooLong,
    EmptyResult,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Idle,
    Recording,
    Transcribing,
    Delivering,
}

impl ServerMessage {
    pub fn error(session: Option<u64>, code: ErrorCode, text: impl Into<String>) -> Self {
        ServerMessage::Error {
            session,
            code,
            text: text.into(),
        }
    }

    /// 序列化成一行 NDJSON (含結尾換行)。
    ///
    /// 轉錄文字本身可能含換行 (SDD §4.5 的安全議題), serde_json 會將其
    /// 跳脫成 `\n`, 因此不會破壞 NDJSON 的分行語意。
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        let mut s = serde_json::to_string(self)?;
        s.push('\n');
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_start() {
        let m: ClientMessage = serde_json::from_str(
            r#"{"type":"start","session":42,"program":"kitty","is_password":false}"#,
        )
        .unwrap();
        match m {
            ClientMessage::Start {
                session,
                program,
                is_password,
            } => {
                assert_eq!(session, 42);
                assert_eq!(program, "kitty");
                assert!(!is_password);
            }
            _ => panic!("wrong variant"),
        }
    }

    /// program 缺席時要退回空字串, 不能讓整筆訊息解析失敗 ——
    /// 有些應用程式的 program() 就是空的。
    #[test]
    fn start_tolerates_missing_optional_fields() {
        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"start","session":1}"#).unwrap();
        match m {
            ClientMessage::Start {
                program,
                is_password,
                ..
            } => {
                assert_eq!(program, "");
                assert!(!is_password);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn parses_fallback_clipboard() {
        let m: ClientMessage =
            serde_json::from_str(r#"{"type":"fallback_clipboard","text":"hi"}"#).unwrap();
        assert!(matches!(m, ClientMessage::FallbackClipboard { .. }));
    }

    #[test]
    fn parses_ping() {
        let m: ClientMessage = serde_json::from_str(r#"{"type":"ping"}"#).unwrap();
        assert!(matches!(m, ClientMessage::Ping));
    }

    /// 欄位名必須與 C++ 端一致。這個測試釘住線上格式,
    /// 改動任一欄位名都會讓它失敗。
    #[test]
    fn result_wire_format() {
        let m = ServerMessage::Result {
            session: 42,
            text: "幫我看一下 git status".into(),
        };
        assert_eq!(
            m.to_line().unwrap(),
            "{\"type\":\"result\",\"session\":42,\"text\":\"幫我看一下 git status\"}\n"
        );
    }

    #[test]
    fn error_wire_format() {
        let m = ServerMessage::error(Some(1), ErrorCode::NoAudioDevice, "找不到麥克風");
        assert_eq!(
            m.to_line().unwrap(),
            "{\"type\":\"error\",\"session\":1,\"code\":\"no_audio_device\",\"text\":\"找不到麥克風\"}\n"
        );
    }

    /// 沒有 session 的錯誤 (例如 fallback_clipboard 失敗) 必須整個略過
    /// session 欄位, 而不是送 null。
    #[test]
    fn error_without_session_omits_field() {
        let m = ServerMessage::error(None, ErrorCode::Internal, "x");
        assert_eq!(
            m.to_line().unwrap(),
            "{\"type\":\"error\",\"code\":\"internal\",\"text\":\"x\"}\n"
        );
    }

    /// 轉錄結果含換行時, 線上必須是跳脫的 `\n`, 否則會被對端當成
    /// 兩則訊息 —— 這是 NDJSON 最容易踩的坑。
    #[test]
    fn newline_in_text_is_escaped_not_raw() {
        let m = ServerMessage::Result {
            session: 1,
            text: "rm -rf /\nyes".into(),
        };
        let line = m.to_line().unwrap();
        assert_eq!(line.matches('\n').count(), 1); // 只有結尾那一個
        assert!(line.contains("\\n"));
    }

    #[test]
    fn pong_wire_format() {
        assert_eq!(ServerMessage::Pong.to_line().unwrap(), "{\"type\":\"pong\"}\n");
    }
}
