//! Binary-size probe: one Parquet count through DuckDB (norn-cell's pin).
fn main() {
    let path = std::env::args().nth(1).unwrap_or_default();
    let conn = duckdb::Connection::open_in_memory().expect("open");
    let n: i64 = conn
        .query_row(
            &format!("SELECT count(*) FROM read_parquet('{}')", path.replace('\'', "''")),
            [],
            |row| row.get(0),
        )
        .expect("count");
    println!("{n}");
}
