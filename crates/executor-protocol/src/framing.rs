use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const DEFAULT_MAX_FRAME_SIZE: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("transport I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("frame length {actual} exceeds maximum {maximum}")]
    TooLarge { actual: usize, maximum: usize },
    #[error("message serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Reads one big-endian u32 length-prefixed JSON message. Clean EOF before a
/// frame header returns `Ok(None)`; EOF inside a frame is an error.
pub async fn read_frame<R, T>(reader: &mut R, maximum: usize) -> Result<Option<T>, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut header = [0_u8; 4];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    }

    let length = u32::from_be_bytes(header) as usize;
    if length > maximum {
        return Err(FrameError::TooLarge {
            actual: length,
            maximum,
        });
    }

    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    Ok(Some(serde_json::from_slice(&payload)?))
}

/// Writes one big-endian u32 length-prefixed JSON message.
pub async fn write_frame<W, T>(writer: &mut W, message: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload = serde_json::to_vec(message)?;
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge {
        actual: payload.len(),
        maximum: u32::MAX as usize,
    })?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ClientMessage, PROTOCOL_VERSION};

    #[tokio::test]
    async fn messages_round_trip_through_length_delimited_json() {
        let expected = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: "test-token".into(),
            workspace: "/home/test/project".into(),
        };
        let (mut client, mut server) = tokio::io::duplex(1024);

        write_frame(&mut client, &expected).await.unwrap();
        let actual = read_frame(&mut server, DEFAULT_MAX_FRAME_SIZE)
            .await
            .unwrap()
            .unwrap();

        assert_eq!(expected, actual);
    }
}
