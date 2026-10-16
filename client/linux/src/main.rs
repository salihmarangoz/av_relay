mod crypto;
mod devtools;
mod proto;
mod store;
mod vcam;

use anyhow::{Result, bail};

const USAGE: &str = "usage: av-relay vcam-test | pair HOST[:PORT] [CODE] | session-test [SECONDS]";

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("vcam-test") => devtools::vcam_test(),
        Some("pair") => devtools::pair(rest),
        Some("session-test") => devtools::session_test(rest),
        _ => bail!(USAGE),
    }
}
