//! Length-delimited JSON framing for sandboxed component workers.

use codypendent_protocol::{UiHardLimits, UiValidationError, UiWireMessage};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Independent cap for one component-runtime message. Tree validation imposes
/// tighter semantic limits after framing; this cap bounds allocation first.
pub const MAX_UI_FRAME_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum UiFramingError {
    #[error("remote UI I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("remote UI frame length {length} exceeds {maximum} bytes")]
    TooLarge { length: usize, maximum: usize },
    #[error("remote UI frame length does not fit the wire header")]
    LengthOverflow,
    #[error("remote UI frame length {length} exceeds the worker byte-rate allowance {available}")]
    ByteBudgetExceeded { length: usize, available: usize },
    #[error("remote UI frame is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("remote UI frame failed validation: {0}")]
    Validation(#[from] UiValidationError),
}

/// Read one big-endian-u32-length JSON message. EOF before a new header is a
/// clean worker shutdown; EOF inside a frame is an I/O error.
pub async fn read_ui_message<R>(reader: &mut R) -> Result<Option<UiWireMessage>, UiFramingError>
where
    R: AsyncRead + Unpin,
{
    read_ui_message_with_limits(reader, &UiHardLimits::default()).await
}

/// Read and validate a message with the hard limits negotiated for this
/// component session.
pub async fn read_ui_message_with_limits<R>(
    reader: &mut R,
    limits: &UiHardLimits,
) -> Result<Option<UiWireMessage>, UiFramingError>
where
    R: AsyncRead + Unpin,
{
    read_ui_message_with_limits_and_gate(reader, limits, |_| Ok(())).await
}

/// Read with a length gate that runs immediately after the four-byte header,
/// before payload allocation, I/O, JSON parsing, or semantic validation.
pub async fn read_ui_message_with_limits_and_gate<R, F>(
    reader: &mut R,
    limits: &UiHardLimits,
    gate: F,
) -> Result<Option<UiWireMessage>, UiFramingError>
where
    R: AsyncRead + Unpin,
    F: FnOnce(usize) -> Result<(), UiFramingError>,
{
    let mut header = [0_u8; 4];
    let first = reader.read(&mut header[..1]).await?;
    if first == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..]).await?;
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_UI_FRAME_BYTES {
        return Err(UiFramingError::TooLarge {
            length,
            maximum: MAX_UI_FRAME_BYTES,
        });
    }
    gate(length)?;
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    let message: UiWireMessage = serde_json::from_slice(&payload)?;
    message.validate(limits)?;
    Ok(Some(message))
}

/// Write one complete message and flush it so a component worker never waits
/// behind a buffered partial frame.
pub async fn write_ui_message<W>(
    writer: &mut W,
    message: &UiWireMessage,
) -> Result<(), UiFramingError>
where
    W: AsyncWrite + Unpin,
{
    message.validate(&UiHardLimits::default())?;
    let payload = serde_json::to_vec(message)?;
    if payload.len() > MAX_UI_FRAME_BYTES {
        return Err(UiFramingError::TooLarge {
            length: payload.len(),
            maximum: MAX_UI_FRAME_BYTES,
        });
    }
    let length = u32::try_from(payload.len()).map_err(|_| UiFramingError::LengthOverflow)?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codypendent_protocol::UiRemoteError;
    use serde_json::Value;
    use std::collections::BTreeMap;

    fn message() -> UiWireMessage {
        UiWireMessage {
            kind: "error".into(),
            message_id: "m1".into(),
            snapshot: None,
            patch_batch: None,
            event: None,
            action: None,
            subscription: None,
            unsubscription: None,
            projection: None,
            action_result: None,
            cancellation: None,
            dispose: None,
            viewport: None,
            resync: None,
            hot_reload: None,
            capabilities: None,
            selection: None,
            contributions: Vec::new(),
            theme: None,
            error: Some(UiRemoteError {
                code: "ui.test".into(),
                message: "safe".into(),
                recoverable: true,
                document_id: None,
                node_id: None,
                patch_index: None,
                recovery: None,
                fallback: None,
                details: Value::Null,
            }),
            extensions: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn framed_message_round_trips() {
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        let expected = message();
        write_ui_message(&mut writer, &expected)
            .await
            .expect("write");
        let actual = read_ui_message(&mut reader)
            .await
            .expect("read")
            .expect("message");
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn byte_budget_rejects_from_header_before_payload_read() {
        let (mut writer, mut reader) = tokio::io::duplex(16);
        writer.write_all(&1024_u32.to_be_bytes()).await.unwrap();
        let error =
            read_ui_message_with_limits_and_gate(&mut reader, &UiHardLimits::default(), |length| {
                Err(UiFramingError::ByteBudgetExceeded {
                    length,
                    available: 64,
                })
            })
            .await
            .expect_err("header should be rejected without waiting for its body");
        assert!(matches!(
            error,
            UiFramingError::ByteBudgetExceeded {
                length: 1024,
                available: 64
            }
        ));
    }

    #[tokio::test]
    async fn oversized_header_is_rejected_before_allocation() {
        let (mut writer, mut reader) = tokio::io::duplex(16);
        writer
            .write_all(&u32::try_from(MAX_UI_FRAME_BYTES + 1).unwrap().to_be_bytes())
            .await
            .unwrap();
        assert!(matches!(
            read_ui_message(&mut reader).await,
            Err(UiFramingError::TooLarge { .. })
        ));
    }

    #[tokio::test]
    async fn partial_header_is_not_treated_as_clean_shutdown() {
        let (mut writer, mut reader) = tokio::io::duplex(16);
        writer.write_all(&[0, 0]).await.unwrap();
        drop(writer);
        assert!(matches!(
            read_ui_message(&mut reader).await,
            Err(UiFramingError::Io(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof
        ));
    }
}
