use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result, anyhow, bail};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::oneshot,
    task::JoinHandle,
};

pub struct CloudflaredTunnel {
    pub public_relay_url: String,
    child: Child,
    log_drain: JoinHandle<()>,
}

impl CloudflaredTunnel {
    pub async fn start(local_http_url: &str) -> Result<Self> {
        let mut child = Command::new("cloudflared")
            .args(["tunnel", "--url", local_http_url, "--no-autoupdate"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("could not start cloudflared; install it or omit --cloudflared")?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("could not read cloudflared output"))?;
        let (url_tx, url_rx) = oneshot::channel();
        let log_drain = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            let mut url_tx = Some(url_tx);
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(url) = find_quick_tunnel_url(&line)
                    && let Some(sender) = url_tx.take()
                {
                    let _ = sender.send(format!(
                        "{}/ws",
                        url.trim_end_matches('/').replacen("https://", "wss://", 1)
                    ));
                }
            }
        });
        let public_relay_url = match tokio::time::timeout(Duration::from_secs(30), url_rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => bail!("cloudflared exited before publishing a tunnel URL"),
            Err(_) => {
                let _ = child.kill().await;
                bail!("timed out waiting for a Cloudflare Quick Tunnel")
            }
        };
        Ok(Self {
            public_relay_url,
            child,
            log_drain,
        })
    }
}

impl Drop for CloudflaredTunnel {
    fn drop(&mut self) {
        self.log_drain.abort();
        let _ = self.child.start_kill();
    }
}

fn find_quick_tunnel_url(line: &str) -> Option<&str> {
    line.split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| {
                matches!(character, ',' | '"' | '\'' | '(' | ')' | '[' | ']')
            })
        })
        .find(|word| word.starts_with("https://") && word.contains(".trycloudflare.com"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_cloudflare_url_from_log_line() {
        let line = "INF | https://paper-river.trycloudflare.com |";
        assert_eq!(
            find_quick_tunnel_url(line),
            Some("https://paper-river.trycloudflare.com")
        );
    }
}
