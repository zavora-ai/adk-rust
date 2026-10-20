//! Configure byte-inactivity limits without a total generation deadline.

use adk_anthropic::Anthropic;
use std::time::Duration;

fn main() -> Result<(), adk_anthropic::Error> {
    let client = Anthropic::new(None)?
        .with_http_timeouts(Duration::from_secs(10), Duration::from_secs(300))?;
    println!("Response-header inactivity limit: {:?}", client.timeout());
    Ok(())
}
