//! Bounded command output and streaming UTF-8 handling.
use crate::harness::progress::{Progress, ProgressSender};
use tokio::io::{AsyncRead, AsyncReadExt};
pub(super) const MAX_OUTPUT: usize = 128 * 1024;

#[cfg(test)]
pub(super) async fn bounded_output(
    input: impl AsyncRead + Unpin,
    progress: Option<ProgressSender>,
) -> std::io::Result<Vec<u8>> {
    let mut result = Vec::new();
    bounded_output_into(input, progress, &mut result).await?;
    Ok(result)
}

pub(super) async fn bounded_output_into(
    mut input: impl AsyncRead + Unpin,
    progress: Option<ProgressSender>,
    result: &mut Vec<u8>,
) -> std::io::Result<()> {
    let mut emitted = 0;
    let mut buffer = [0u8; 8192];
    let mut truncated = false;
    loop {
        let count = input.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let keep = count.min(MAX_OUTPUT.saturating_sub(result.len()));
        result.extend_from_slice(&buffer[..keep]);
        // Retain an incomplete UTF-8 suffix until the next read so a terminal
        // never receives a replacement character for a split Unicode scalar.
        let end = emitted + complete_utf8_prefix(&result[emitted..]);
        send_output(progress.as_ref(), &result[emitted..end]);
        emitted = end;
        truncated |= keep < count;
    }
    if truncated {
        result.extend_from_slice(b"\n[output truncated]\n");
    }
    send_output(progress.as_ref(), &result[emitted..]);
    Ok(())
}

fn send_output(progress: Option<&ProgressSender>, bytes: &[u8]) {
    if let Some(progress) = progress.filter(|_| !bytes.is_empty()) {
        let _ = progress.send(Progress::CommandOutput(
            String::from_utf8_lossy(bytes).into_owned(),
        ));
    }
}

fn complete_utf8_prefix(bytes: &[u8]) -> usize {
    let mut offset = 0;
    while offset < bytes.len() {
        match std::str::from_utf8(&bytes[offset..]) {
            Ok(_) => return bytes.len(),
            Err(error) => {
                offset += error.valid_up_to();
                match error.error_len() {
                    Some(invalid) => offset += invalid,
                    None => return offset,
                }
            }
        }
    }
    offset
}
