//! Binary-size probe: one Parquet count through DataFusion 55.3 (SQL,
//! Parquet with encryption, Substrait), the features a lake engine needs.
use datafusion::prelude::*;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_default();
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    runtime.block_on(async {
        let ctx = SessionContext::new();
        ctx.register_parquet("t", &path, ParquetReadOptions::default())
            .await
            .expect("register");
        let frame = ctx.sql("SELECT count(*) FROM t").await.expect("sql");
        let plan = frame.logical_plan().clone();
        // Keep the Substrait producer linked, as a typed-plan lake would.
        let produced = datafusion_substrait::logical_plan::producer::to_substrait_plan(&plan, &ctx.state());
        let batches = frame.collect().await.expect("collect");
        println!("{} {}", batches[0].num_rows(), produced.is_ok());
    });
}
