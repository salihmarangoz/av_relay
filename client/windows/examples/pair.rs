// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless pairing for diagnostics. Reads the one-time code from stdin.
use anyhow::{Context, Result};
use av_relay_windows::{
    proto::pairing::{self, ClientInfo},
    store::{PairedSender, Store},
};
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let host = args
        .get(1)
        .context("Usage: pair HOST [PORT]; enter pairing code on stdin")?;
    let port = args
        .get(2)
        .map(|p| p.parse::<u16>())
        .transpose()?
        .unwrap_or(48100);
    let mut code = String::new();
    std::io::stdin().read_line(&mut code)?;
    let store = Store::open_default()?;
    let identity = store.identity()?;
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await??;
    let paired = pairing::start(stream)
        .await?
        .finish(
            code.trim(),
            &identity,
            &ClientInfo {
                name: std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Windows PC".into()),
                app: "av-relay windows diagnostic".into(),
            },
        )
        .await?;
    store.save_paired(&PairedSender {
        public_key: paired
            .public_key
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
        device_id: paired.device_id,
        name: paired.name.clone(),
        address: host.clone(),
        port,
    })?;
    println!("Paired with {}", paired.name);
    Ok(())
}
