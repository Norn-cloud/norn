//! Native transcript oracle for real-runtime correspondence.
use norn_api::{mcp_host::{Host, Request}, scope::CellScope};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut host = Host::new(CellScope { org_id: "demo".into(), environment_id: "dev".into() }, "http://127.0.0.1:9876".into());
    let requests: Vec<(Request, String, u64)> = serde_json::from_reader(std::io::stdin())?;
    let responses: Vec<_> = requests.into_iter().map(|(r, nonce, now)| host.handle(r, &nonce, now)).collect();
    println!("{}", serde_json::to_string(&responses)?);
    Ok(())
}
