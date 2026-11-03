//! Native transcript oracle for real-runtime correspondence.
use norn_api::{mcp_host::{Host, Request}, scope::CellScope};
use std::io::{BufRead as _, Write as _};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(CellScope { org_id: "demo".into(), environment_id: "dev".into() }, "http://127.0.0.1:9876".into());
    for line in std::io::stdin().lock().lines() {
        let (request, nonce, now): (Request, String, u64) = serde_json::from_str(&line?)?;
        writeln!(std::io::stdout().lock(), "{}", serde_json::to_string(&host.handle(request, &nonce, now))?)?;
        std::io::stdout().flush()?;
    }
    Ok(())
}
