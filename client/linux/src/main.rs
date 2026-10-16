mod audio;
mod crypto;
mod devtools;
mod media;
mod proto;
mod store;
mod vcam;

use anyhow::{Result, bail};

const USAGE: &str =
    "usage: av-relay vcam-test | vmic-test [SECONDS] [--default] [--drift PPM] | media-test [SECONDS] | answer-test OFFER.sdp | pair HOST[:PORT] [CODE] | session-test [SECONDS]";

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or_default();
    match args.first().map(String::as_str) {
        Some("vcam-test") => devtools::vcam_test(),
        Some("vmic-test") => devtools::vmic_test(rest),
        Some("media-test") => devtools::media_test(rest),
        Some("answer-test") => devtools::answer_test(rest),
        Some("pair") => devtools::pair(rest),
        Some("session-test") => devtools::session_test(rest),
        _ => bail!(USAGE),
    }
}
