//! Put text on the human's clipboard from the TUI.
use anyhow::{Context, Result};
use base64::Engine;
use std::{
    io::Write,
    process::{Command, Stdio},
};

const MAX_BYTES: usize = 1024 * 1024;
/// Terminals cap one OSC 52 sequence near 100 KB; stay under it after base64.
const MAX_OSC52_BYTES: usize = 74 * 1024;

#[cfg(target_os = "macos")]
const TOOLS: [(&str, &[&str]); 1] = [("/usr/bin/pbcopy", &[])];
#[cfg(not(target_os = "macos"))]
const TOOLS: [(&str, &[&str]); 3] = [
    ("wl-copy", &[]),
    ("xclip", &["-selection", "clipboard"]),
    ("xsel", &["--clipboard", "--input"]),
];

/// Where the text went, so the notice can say when delivery depends on the
/// terminal honouring the escape sequence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    Tool,
    Terminal,
}

/// A platform clipboard tool when the session is local, because Terminal.app
/// implements no OSC 52; the escape sequence over SSH, where a tool would
/// fill the remote machine's clipboard, and wherever no tool exists.
pub(super) fn copy(text: &str) -> Result<Route> {
    anyhow::ensure!(
        text.len() <= MAX_BYTES,
        "selection exceeds the 1 MiB clipboard limit"
    );
    let local = std::env::var_os("SSH_CONNECTION").is_none();
    if local
        && TOOLS
            .iter()
            .any(|(tool, args)| pipe(tool, args, text).is_ok())
    {
        return Ok(Route::Tool);
    }
    anyhow::ensure!(
        text.len() <= MAX_OSC52_BYTES,
        "selection is too large to copy through the terminal"
    );
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(osc52(text).as_bytes())?;
    stdout.flush().context("write clipboard escape sequence")?;
    Ok(Route::Terminal)
}

fn pipe(tool: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = Command::new(tool)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    child
        .stdin
        .take()
        .context("clipboard tool has no stdin")?
        .write_all(text.as_bytes())?;
    anyhow::ensure!(child.wait()?.success(), "{tool} failed");
    Ok(())
}

fn osc52(text: &str) -> String {
    format!(
        "\x1b]52;c;{}\x07",
        base64::engine::general_purpose::STANDARD.encode(text)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_carries_base64_text_for_the_clipboard_selection() {
        assert_eq!(osc52("hi\n"), "\x1b]52;c;aGkK\x07");
    }

    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "overwrites the real clipboard"]
    fn pbcopy_round_trips_unicode_text() {
        let text = "moosedev 🫎 selection\nsecond line";
        pipe(TOOLS[0].0, TOOLS[0].1, text).unwrap();
        let pasted = Command::new("/usr/bin/pbpaste").output().unwrap().stdout;
        assert_eq!(String::from_utf8(pasted).unwrap(), text);
    }
}
