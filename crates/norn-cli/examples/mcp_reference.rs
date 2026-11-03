//! Native oracle for the real-celld transcript.
#[path = "mcp-workers/backend.rs"]
mod backend;
use norn_api::mcp_host::Request;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = backend::host("demo".into(), "dev".into(), "http://127.0.0.1:9876".into())
        .map_err(|e| e.explanation)?;
    let requests: Vec<(Request, String, u64)> = serde_json::from_reader(std::io::stdin())?;
    let responses: Vec<_> = requests
        .into_iter()
        .map(|(r, nonce, now)| host.handle(&r, &nonce, now))
        .collect();
    serde_json::to_writer(std::io::stdout(), &responses).map_err(Into::into)
}
