//! Best-effort local desktop notifications. The journal remains authoritative.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

pub type NotifyFuture<'a> = Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'a>>;

pub trait Notifier: Send {
    fn notify<'a>(&'a self, title: &'a str, body: &'a str) -> NotifyFuture<'a>;
}

/// Uses the operator's local Linux desktop session, never a hosted service.
pub struct DesktopNotifier;

impl Notifier for DesktopNotifier {
    fn notify<'a>(&'a self, title: &'a str, body: &'a str) -> NotifyFuture<'a> {
        Box::pin(send(title, body))
    }
}

async fn send(title: &str, body: &str) -> io::Result<()> {
    send_command(
        Command::new("notify-send"),
        title,
        body,
        Duration::from_secs(5),
    )
    .await
}

async fn send_command(
    mut command: Command,
    title: &str,
    body: &str,
    timeout: Duration,
) -> io::Result<()> {
    // No shell interpolation, inherited stdin, or helper output in the journal.
    command
        .args(["--app-name=limen-live", "--", title, body])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let status = tokio::time::timeout(timeout, command.status())
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "notify-send timed out"))??;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "notify-send exited with {status}"
        )))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn passes_literal_arguments_once_to_local_helper() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("args");
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf '%s\\n' \"$@\" > \"$0\""]);
        command.arg(&output);
        send_command(
            command,
            "fill",
            "BTC-USD $(false); 'literal'",
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            "--app-name=limen-live\n--\nfill\nBTC-USD $(false); 'literal'\n"
        );
    }

    #[tokio::test]
    async fn reports_missing_failed_and_stalled_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let err = send_command(
            Command::new(dir.path().join("missing")),
            "halt",
            "reason",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let mut failed = Command::new("/bin/sh");
        failed.args(["-c", "exit 1"]);
        assert!(
            send_command(failed, "halt", "reason", Duration::from_secs(1))
                .await
                .is_err()
        );
        let mut stalled = Command::new("/bin/sh");
        stalled.args(["-c", "exec sleep 10"]);
        let err = send_command(stalled, "halt", "reason", Duration::from_millis(20))
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    }
}
