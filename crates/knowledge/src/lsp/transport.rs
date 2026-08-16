//! LSP base-protocol framing: `Content-Length: N\r\n\r\n{json}` over any
//! async byte stream. Generic so tests drive it over `tokio::io::duplex`.

use serde::{Deserialize, Serialize};
use std::io::{Error, ErrorKind};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

/// One incoming JSON-RPC message, minimally decoded: requests carry `id` +
/// `method`, notifications only `method`, responses only `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Incoming {
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<serde_json::Value>,
    #[serde(default)]
    pub result: Option<serde_json::Value>,
    #[serde(default)]
    pub error: Option<serde_json::Value>,
}

pub struct Transport<R, W> {
    reader: BufReader<R>,
    writer: W,
    next_id: i64,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Transport<R, W> {
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
            next_id: 1,
        }
    }

    /// Send a request; returns the id used.
    pub async fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> std::io::Result<i64> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_json(&payload).await?;
        Ok(id)
    }

    /// Send a notification (no id).
    pub async fn notify(&mut self, method: &str, params: serde_json::Value) -> std::io::Result<()> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_json(&payload).await
    }

    /// Send a response for a server request.
    pub async fn respond(
        &mut self,
        id: serde_json::Value,
        result: serde_json::Value,
    ) -> std::io::Result<()> {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        self.write_json(&payload).await
    }

    async fn write_json(&mut self, val: &serde_json::Value) -> std::io::Result<()> {
        let body = serde_json::to_vec(val)
            .map_err(|e| Error::new(ErrorKind::InvalidData, e.to_string()))?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.writer.write_all(header.as_bytes()).await?;
        self.writer.write_all(&body).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Read one framed message. Header parsing tolerates the optional
    /// `Content-Type` header and unknown headers; a malformed frame is an error.
    pub async fn read(&mut self) -> std::io::Result<Incoming> {
        let mut content_length: Option<usize> = None;
        let mut line = String::new();

        loop {
            line.clear();
            let n = self.reader.read_line(&mut line).await?;
            if n == 0 {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "unexpected EOF while reading headers",
                ));
            }

            let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
            if trimmed.is_empty() {
                // End of header section
                break;
            }

            if let Some((name, val)) = trimmed.split_once(':') {
                let name = name.trim();
                let val = val.trim();
                if name.eq_ignore_ascii_case("content-length") {
                    let len = val.parse::<usize>().map_err(|_| {
                        Error::new(ErrorKind::InvalidData, "invalid Content-Length header")
                    })?;
                    content_length = Some(len);
                }
                // Tolerate Content-Type or other headers
            }
        }

        let length = content_length
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "missing Content-Length header"))?;

        const MAX_LSP_MESSAGE_BYTES: usize = 32 * 1024 * 1024;
        if length > MAX_LSP_MESSAGE_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("Content-Length {length} exceeds maximum allowed size of {MAX_LSP_MESSAGE_BYTES} bytes"),
            ));
        }

        let mut body = vec![0u8; length];
        self.reader.read_exact(&mut body).await?;

        let incoming: Incoming = serde_json::from_slice(&body)
            .map_err(|e| Error::new(ErrorKind::InvalidData, format!("invalid JSON: {e}")))?;
        Ok(incoming)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn frames_round_trip_over_duplex() {
        let (client_io, server_io) = duplex(1024);
        let (cr, cw) = tokio::io::split(client_io);
        let (sr, sw) = tokio::io::split(server_io);

        let mut client_t = Transport::new(cr, cw);
        let mut server_t = Transport::new(sr, sw);

        let req_id = client_t
            .request("test/method", serde_json::json!({"foo": "bar"}))
            .await
            .unwrap();
        assert_eq!(req_id, 1);

        let msg = server_t.read().await.unwrap();
        assert_eq!(msg.id, Some(serde_json::json!(1)));
        assert_eq!(msg.method.as_deref(), Some("test/method"));
        assert_eq!(msg.params.unwrap(), serde_json::json!({"foo": "bar"}));

        server_t
            .respond(msg.id.unwrap(), serde_json::json!({"ok": true}))
            .await
            .unwrap();

        let resp = client_t.read().await.unwrap();
        assert_eq!(resp.id, Some(serde_json::json!(1)));
        assert_eq!(resp.result.unwrap(), serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn tolerates_content_type_header() {
        let (client_io, server_io) = duplex(1024);
        let (_cr, mut cw) = tokio::io::split(client_io);
        let (sr, _sw) = tokio::io::split(server_io);

        let mut server_t = Transport::new(sr, _sw);

        let payload = r#"{"jsonrpc":"2.0","method":"custom/notify","params":{}}"#;
        let frame = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\nCustom-Header: value\r\n\r\n{}",
            payload.len(),
            payload
        );
        cw.write_all(frame.as_bytes()).await.unwrap();
        cw.flush().await.unwrap();

        let msg = server_t.read().await.unwrap();
        assert_eq!(msg.method.as_deref(), Some("custom/notify"));
    }

    #[tokio::test]
    async fn malformed_frame_is_an_error() {
        let (client_io, server_io) = duplex(1024);
        let (_cr, mut cw) = tokio::io::split(client_io);
        let (sr, sw) = tokio::io::split(server_io);

        let mut server_t = Transport::new(sr, sw);

        cw.write_all(b"No-Content-Length: header\r\n\r\n{}")
            .await
            .unwrap();
        cw.flush().await.unwrap();

        let err = server_t.read().await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn request_ids_are_monotonic() {
        let (client_io, server_io) = duplex(1024);
        let (cr, cw) = tokio::io::split(client_io);
        let (sr, sw) = tokio::io::split(server_io);

        let mut client_t = Transport::new(cr, cw);
        let _server_t = Transport::new(sr, sw);

        let id1 = client_t.request("m1", serde_json::json!({})).await.unwrap();
        let id2 = client_t.request("m2", serde_json::json!({})).await.unwrap();
        let id3 = client_t.request("m3", serde_json::json!({})).await.unwrap();

        assert_eq!(id1, 1);
        assert_eq!(id2, 2);
        assert_eq!(id3, 3);
    }
}
