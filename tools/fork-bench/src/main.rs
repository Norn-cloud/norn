//! Fork micro-benchmark (#3148 / D81). Experiment, not production code.
//!
//! Compares a content-addressed copy-on-write page VFS (`vfs.rs`) against
//! forking by file copy and by filesystem reflink, on a synthetic cell-shaped
//! SQLite database (versioned facts + unique key index + hash-chained ledger).
//!
//! Configuration is by environment variable; see `Cfg::from_env`.

mod vfs;

use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};
use rusqlite::{params, Connection, OpenFlags};

use vfs::{engine, Engine, PAGE};

struct Cfg {
    size_mb: u64,
    forks: Vec<usize>,
    dir: PathBuf,
    durable: bool,
    threads: usize,
    commits: usize,
    copies: usize,
    reflink_dir: Option<PathBuf>,
    reads: usize,
    fork_cache_kib: i64,
}

fn env<T: std::str::FromStr>(k: &str, d: T) -> T {
    std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d)
}

impl Cfg {
    fn from_env() -> Self {
        Self {
            size_mb: env("FB_SIZE_MB", 256),
            forks: std::env::var("FB_FORKS")
                .unwrap_or_else(|_| "20,100,1000".into())
                .split(',')
                .map(|s| s.trim().parse().unwrap())
                .collect(),
            dir: PathBuf::from(std::env::var("FB_DIR").unwrap_or_else(|_| "/tmp/fork-bench".into())),
            durable: env("FB_DURABLE", 1u8) == 1,
            threads: env("FB_THREADS", 16),
            commits: env("FB_COMMITS", 20),
            copies: env("FB_COPIES", 20),
            reflink_dir: std::env::var("FB_REFLINK_DIR").ok().map(PathBuf::from),
            reads: env("FB_READS", 20_000),
            fork_cache_kib: env("FB_FORK_CACHE_KIB", 256),
        }
    }
}

// ------------------------------------------------------------------ stats

fn pct(v: &mut [u64], p: f64) -> u64 {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i]
}

fn fmt_ns(ns: u64) -> String {
    if ns >= 1_000_000 {
        format!("{:.2} ms", ns as f64 / 1e6)
    } else if ns >= 1_000 {
        format!("{:.1} µs", ns as f64 / 1e3)
    } else {
        format!("{ns} ns")
    }
}

fn summary(label: &str, v: &mut Vec<u64>) {
    let n = v.len();
    let p50 = pct(v, 0.50);
    let p99 = pct(v, 0.99);
    let max = *v.iter().max().unwrap_or(&0);
    println!("  {label:<44} n={n:<6} p50={:<10} p99={:<10} max={}", fmt_ns(p50), fmt_ns(p99), fmt_ns(max));
}

fn rss_mib() -> f64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines()
        .find(|l| l.starts_with("VmRSS:"))
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|kb| kb.parse::<f64>().ok())
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn mib(b: u64) -> String {
    format!("{:.2} MiB", b as f64 / (1024.0 * 1024.0))
}

// --------------------------------------------------------------- workload

const TYPES: u64 = 10;
const PROPS: u64 = 8;

fn text(rng: &mut SmallRng, n: usize) -> String {
    (0..n).map(|_| (b'a' + rng.gen_range(0..26)) as char).collect()
}

fn schema(c: &Connection) {
    c.execute_batch(
        "CREATE TABLE facts(
            id INTEGER PRIMARY KEY, world TEXT NOT NULL, object_type TEXT NOT NULL,
            subject TEXT NOT NULL, property TEXT NOT NULL, version INTEGER NOT NULL,
            value TEXT NOT NULL);
         CREATE UNIQUE INDEX facts_key ON facts(world, object_type, subject, property, version);
         CREATE TABLE ledger(seq INTEGER PRIMARY KEY, prev BLOB NOT NULL, hash BLOB NOT NULL, body TEXT NOT NULL);",
    )
    .unwrap();
}

fn populate(c: &Connection, subjects: u64) {
    let mut rng = SmallRng::seed_from_u64(7);
    let batch = 5_000u64;
    let mut i = 0u64;
    let mut prev = [0u8; 32];
    while i < subjects {
        c.execute_batch("BEGIN").unwrap();
        {
            let mut f = c.prepare_cached("INSERT INTO facts(world,object_type,subject,property,version,value) VALUES('main',?1,?2,?3,1,?4)").unwrap();
            let mut l = c.prepare_cached("INSERT INTO ledger(prev,hash,body) VALUES(?1,?2,?3)").unwrap();
            for s in i..(i + batch).min(subjects) {
                for p in 0..PROPS {
                    f.execute(params![format!("t{}", s % TYPES), format!("s{s:09}"), format!("p{p}"), text(&mut rng, 100)]).unwrap();
                }
                let body = text(&mut rng, 300);
                let h = *blake3::hash(&[&prev[..], body.as_bytes()].concat()).as_bytes();
                l.execute(params![&prev[..], &h[..], body]).unwrap();
                prev = h;
            }
        }
        c.execute_batch("COMMIT").unwrap();
        i += batch;
    }
}

/// One agent/main commit: append a new fact version and a ledger event.
fn commit_one(c: &Connection, rng: &mut SmallRng, subjects: u64, version: i64) {
    let s = rng.gen_range(0..subjects);
    let p = rng.gen_range(0..PROPS);
    c.execute_batch("BEGIN").unwrap();
    c.prepare_cached("INSERT INTO facts(world,object_type,subject,property,version,value) VALUES('main',?1,?2,?3,?4,?5)")
        .unwrap()
        .execute(params![format!("t{}", s % TYPES), format!("s{s:09}"), format!("p{p}"), version, text(rng, 100)])
        .unwrap();
    c.prepare_cached("INSERT INTO ledger(prev,hash,body) VALUES(x'00',x'00',?1)")
        .unwrap()
        .execute(params![text(rng, 300)])
        .unwrap();
    c.execute_batch("COMMIT").unwrap();
}

fn point_reads(c: &Connection, subjects: u64, n: usize, seed: u64) -> Vec<u64> {
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut q = c
        .prepare_cached("SELECT value FROM facts WHERE world='main' AND object_type=?1 AND subject=?2 AND property=?3 AND version=1")
        .unwrap();
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let s = rng.gen_range(0..subjects);
        let p = rng.gen_range(0..PROPS);
        let t = Instant::now();
        let v: String = q.query_row(params![format!("t{}", s % TYPES), format!("s{s:09}"), format!("p{p}")], |r| r.get(0)).unwrap();
        out.push(t.elapsed().as_nanos() as u64);
        assert_eq!(v.len(), 100);
    }
    out
}

fn pragmas(c: &Connection, cache_kib: i64, cow: bool) {
    if cow {
        c.execute_batch("PRAGMA journal_mode=MEMORY; PRAGMA locking_mode=EXCLUSIVE; PRAGMA temp_store=MEMORY;").unwrap();
    }
    c.execute_batch(&format!("PRAGMA synchronous=NORMAL; PRAGMA cache_size=-{cache_kib};")).unwrap();
}

fn open_cow(name: &str, cache_kib: i64) -> Connection {
    let c = Connection::open_with_flags_and_vfs(
        name,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        "cow",
    )
    .unwrap();
    pragmas(&c, cache_kib, true);
    c
}

static MAIN_VERSION: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(1_000_000);

/// fdatasync latency of a 4 KiB append in `dir`: the floor under any durable
/// commit or durable fork record on this disk.
fn fsync_probe(dir: &Path) {
    use std::io::Write;
    let p = dir.join("fsync-probe");
    let mut f = std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(&p).unwrap();
    let buf = [7u8; 4096];
    let mut v = Vec::new();
    for _ in 0..50 {
        let t = Instant::now();
        f.write_all(&buf).unwrap();
        f.sync_data().unwrap();
        v.push(t.elapsed().as_nanos() as u64);
    }
    summary(&format!("fdatasync 4 KiB append on {}", dir.display()), &mut v);
    let _ = std::fs::remove_file(p);
}

/// Main keeps committing while forks are created and agents write.
fn spawn_main_writer(main: Connection, subjects: u64, stop: Arc<AtomicBool>, pace: Duration) -> std::thread::JoinHandle<(Connection, Vec<u64>)> {
    std::thread::spawn(move || {
        let mut rng = SmallRng::seed_from_u64(99);
        let mut lat = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            let v = MAIN_VERSION.fetch_add(1, Ordering::Relaxed);
            let t = Instant::now();
            commit_one(&main, &mut rng, subjects, v);
            lat.push(t.elapsed().as_nanos() as u64);
            std::thread::sleep(pace);
        }
        (main, lat)
    })
}

// -------------------------------------------------------------- baselines

fn ficlone(src: &Path, dst: &Path) -> std::io::Result<()> {
    const FICLONE: libc::c_ulong = 0x4004_9409;
    let s = std::fs::File::open(src)?;
    let d = std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(dst)?;
    let rc = unsafe { libc::ioctl(d.as_raw_fd(), FICLONE as _, s.as_raw_fd()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    d.sync_all()
}

fn baselines(cfg: &Cfg, subjects: u64) {
    println!("\n== Baselines: native SQLite file (unix VFS) ==");
    let path = cfg.dir.join("native.db");
    let _ = std::fs::remove_file(&path);
    let t = Instant::now();
    let c = Connection::open(&path).unwrap();
    pragmas(&c, 2000, false);
    schema(&c);
    populate(&c, subjects);
    let size = std::fs::metadata(&path).unwrap().len();
    println!("  built native.db: {} in {:.1}s", mib(size), t.elapsed().as_secs_f64());
    let mut r = point_reads(&c, subjects, cfg.reads, 1);
    summary("native main point read (cache 2 MiB)", &mut r);
    let mut lat = Vec::new();
    let mut rng = SmallRng::seed_from_u64(5);
    for i in 0..200 {
        let t = Instant::now();
        commit_one(&c, &mut rng, subjects, 2_000_000 + i);
        lat.push(t.elapsed().as_nanos() as u64);
    }
    summary("native main commit (synchronous=NORMAL, fsync)", &mut lat);
    drop(c);

    // Fork by full copy + fsync: what "just copy the file" costs.
    let mut copy = Vec::new();
    for i in 0..cfg.copies {
        let dst = cfg.dir.join(format!("copy-{i}.db"));
        let t = Instant::now();
        std::fs::copy(&path, &dst).unwrap();
        std::fs::File::open(&dst).unwrap().sync_all().unwrap();
        copy.push(t.elapsed().as_nanos() as u64);
        std::fs::remove_file(&dst).unwrap();
    }
    summary(&format!("fork by file copy+fsync ({})", mib(size)), &mut copy);
    println!("  storage per copy fork: {} (100% of the cell)", mib(size));

    // Fork by reflink (FICLONE), if the filesystem supports it.
    let rdir = cfg.reflink_dir.clone().unwrap_or_else(|| cfg.dir.clone());
    let rsrc = rdir.join("reflink-src.db");
    if rdir != cfg.dir {
        std::fs::copy(&path, &rsrc).unwrap();
    } else {
        std::fs::hard_link(&path, &rsrc).ok();
    }
    match ficlone(&rsrc, &rdir.join("probe.db")) {
        Err(e) => println!("  reflink on {}: unsupported ({e})", rdir.display()),
        Ok(()) => {
            let _ = std::fs::remove_file(rdir.join("probe.db"));
            for &n in &cfg.forks {
                let mut v = Vec::new();
                for i in 0..n {
                    let dst = rdir.join(format!("rl-{i}.db"));
                    let t = Instant::now();
                    ficlone(&rsrc, &dst).unwrap();
                    v.push(t.elapsed().as_nanos() as u64);
                }
                summary(&format!("fork by reflink+fsync, {n} forks"), &mut v);
                for i in 0..n {
                    let _ = std::fs::remove_file(rdir.join(format!("rl-{i}.db")));
                }
            }
        }
    }
    let _ = std::fs::remove_file(&rsrc);
    let _ = std::fs::remove_file(&path);
}

// ------------------------------------------------------------------- cow

fn main() {
    let cfg = Cfg::from_env();
    std::fs::create_dir_all(&cfg.dir).unwrap();
    let cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    println!(
        "fork-bench: size={} MiB forks={:?} durable={} threads={} commits/fork={} cores={} sqlite={}",
        cfg.size_mb, cfg.forks, cfg.durable, cfg.threads, cfg.commits, cores, rusqlite::version()
    );
    // ~2.6 KB per subject (8 fact rows + index entries + one ledger event).
    let subjects = cfg.size_mb * 1024 * 1024 / 2600;

    fsync_probe(&cfg.dir);
    if let Some(r) = &cfg.reflink_dir {
        fsync_probe(r);
    }
    let only = std::env::var("FB_PHASES").unwrap_or_else(|_| "baseline,cow".into());
    if only.contains("baseline") {
        baselines(&cfg, subjects);
    }
    if !only.contains("cow") {
        return;
    }

    vfs::register(cfg.durable.then(|| cfg.dir.join("cow")).as_deref());
    let e = engine();
    println!("\n== CoW page VFS ==");
    let t = Instant::now();
    let main = open_cow("main", 2000);
    schema(&main);
    populate(&main, subjects);
    let pages = e.file("main").unwrap().pages();
    println!(
        "  built main: {} pages = {} in {:.1}s; store {} unique; RSS {:.0} MiB",
        pages,
        mib((pages * PAGE) as u64),
        t.elapsed().as_secs_f64(),
        mib(e.store.bytes()),
        rss_mib()
    );
    let mut r = point_reads(&main, subjects, cfg.reads, 1);
    summary("cow main point read (cache 2 MiB)", &mut r);

    // Main-only commit baseline.
    let stop = Arc::new(AtomicBool::new(false));
    let h = spawn_main_writer(main, subjects, stop.clone(), Duration::from_millis(2));
    std::thread::sleep(Duration::from_secs(2));
    stop.store(true, Ordering::Relaxed);
    let (mut main, mut lat) = h.join().unwrap();
    summary("cow main commit, no forks", &mut lat);

    for &n in &cfg.forks {
        main = round(&cfg, main, subjects, n);
    }
    chain(&cfg, &main, subjects);
    dedupe(&cfg, subjects);
    if let Some(d) = &e.durable {
        println!(
            "\n  durable files: pack {} , root log {} , fsyncs {}",
            mib(d.pack_bytes.load(Ordering::Relaxed)),
            mib(d.meta_bytes.load(Ordering::Relaxed)),
            d.fsyncs.load(Ordering::Relaxed)
        );
    }
    drop(main);
    let _ = std::fs::remove_dir_all(cfg.dir.join("cow"));
}

/// One round at `n` concurrent forks: create, open, agents write, read, GC.
fn round(cfg: &Cfg, main: Connection, subjects: u64, n: usize) -> Connection {
    let e = engine();
    println!("\n-- {n} forks --");
    let store_before = e.store.bytes();
    let meta_before = e.durable.as_ref().map(|d| d.meta_bytes.load(Ordering::Relaxed)).unwrap_or(0);

    // Main keeps writing throughout the round.
    let stop = Arc::new(AtomicBool::new(false));
    let mw = spawn_main_writer(main, subjects, stop.clone(), Duration::from_millis(2));

    // 1. Create n forks concurrently on `threads` threads.
    let next = Arc::new(AtomicU64::new(0));
    let t0 = Instant::now();
    let creators: Vec<_> = (0..cfg.threads)
        .map(|_| {
            let next = next.clone();
            std::thread::spawn(move || {
                let mut out = Vec::new();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed) as usize;
                    if i >= n {
                        break;
                    }
                    let (d, base) = engine().fork("main", &format!("f{n}-{i}"));
                    out.push((i, d.as_nanos() as u64, base));
                }
                out
            })
        })
        .collect();
    let mut created: Vec<_> = creators.into_iter().flat_map(|h| h.join().unwrap()).collect();
    let wall = t0.elapsed();
    created.sort_by_key(|x| x.0);
    let mut fl: Vec<u64> = created.iter().map(|x| x.1).collect();
    summary(&format!("fork create ({} threads, durable={})", cfg.threads, cfg.durable), &mut fl);
    println!("  {n} forks created in {:.2} ms wall; RSS {:.0} MiB", wall.as_secs_f64() * 1e3, rss_mib());
    let bases: Vec<_> = created.into_iter().map(|x| x.2).collect();

    // 2. Open every fork (all stay open) and time open + first read.
    let mut open_lat = Vec::new();
    let mut conns = Vec::with_capacity(n);
    for i in 0..n {
        let t = Instant::now();
        let c = open_cow(&format!("f{n}-{i}"), cfg.fork_cache_kib);
        let _: i64 = c.query_row("SELECT count(*) FROM ledger WHERE seq < 10", [], |r| r.get(0)).unwrap();
        open_lat.push(t.elapsed().as_nanos() as u64);
        conns.push(c);
    }
    summary("fork open + first query", &mut open_lat);
    println!("  {n} forks open; RSS {:.0} MiB", rss_mib());

    // 3. Agents: every fork commits `commits` times, interleaved round-robin
    //    across threads so all n forks progress concurrently.
    let mut buckets: Vec<Vec<(usize, Connection)>> = (0..cfg.threads).map(|_| Vec::new()).collect();
    for (i, c) in conns.into_iter().enumerate() {
        buckets[i % cfg.threads].push((i, c));
    }
    let commits = cfg.commits;
    let t1 = Instant::now();
    let agents: Vec<_> = buckets
        .into_iter()
        .enumerate()
        .map(|(ti, b)| {
            std::thread::spawn(move || {
                let mut rng = SmallRng::seed_from_u64(1000 + ti as u64);
                let mut lat = Vec::new();
                for k in 0..commits {
                    for (_, c) in &b {
                        let t = Instant::now();
                        commit_one(c, &mut rng, subjects, 10 + k as i64);
                        lat.push(t.elapsed().as_nanos() as u64);
                    }
                }
                (b, lat)
            })
        })
        .collect();
    let mut all_lat = Vec::new();
    let mut conns: Vec<(usize, Connection)> = Vec::new();
    for h in agents {
        let (b, lat) = h.join().unwrap();
        all_lat.extend(lat);
        conns.extend(b);
    }
    let awall = t1.elapsed();
    stop.store(true, Ordering::Relaxed);
    let (main, mut mlat) = mw.join().unwrap();
    summary(&format!("fork commit ({} commits total)", n * commits), &mut all_lat);
    println!("  agent phase {:.2}s wall, {:.0} fork commits/s", awall.as_secs_f64(), (n * commits) as f64 / awall.as_secs_f64());
    summary("main commit during fork create + agents", &mut mlat);

    // 4. Storage: exact per-fork divergence and store growth.
    let mut div: Vec<u64> = (0..n)
        .map(|i| Engine::diverged(&bases[i], &e.committed(&format!("f{n}-{i}"))) as u64)
        .collect();
    let mean = div.iter().sum::<u64>() as f64 / n as f64;
    let store_growth = e.store.bytes() - store_before;
    let meta_growth = e.durable.as_ref().map(|d| d.meta_bytes.load(Ordering::Relaxed)).unwrap_or(0) - meta_before;
    let db = (e.file("main").unwrap().pages() * PAGE) as u64;
    println!(
        "  pages diverged per fork after {commits} commits: mean {:.1}, p99 {}, max {} (= {} mean per fork)",
        mean,
        pct(&mut div, 0.99),
        div.iter().max().unwrap(),
        mib((mean * PAGE as f64) as u64)
    );
    println!(
        "  store growth (all forks + main): {} = {:.4}% of {n} full copies ({}); root-log growth {}",
        mib(store_growth),
        100.0 * store_growth as f64 / (n as f64 * db as f64),
        mib(n as u64 * db),
        mib(meta_growth)
    );

    // 5. Read latency in forks vs main (same cache size for both).
    let mut fr = Vec::new();
    for (_, c) in conns.iter().take(5) {
        c.execute_batch("PRAGMA cache_size=-2000").unwrap();
        fr.extend(point_reads(c, subjects, cfg.reads / 5, 1));
    }
    summary("fork point read (cache 2 MiB)", &mut fr);
    let mut mr = point_reads(&main, subjects, cfg.reads, 1);
    summary("main point read now (cache 2 MiB)", &mut mr);
    println!("  RSS with {n} forks open after writes: {:.0} MiB", rss_mib());

    // 6. Discard every fork, then GC by reachability.
    drop(conns);
    for i in 0..n {
        e.drop_file(&format!("f{n}-{i}"));
    }
    let (live, freed, gct) = e.gc();
    println!(
        "  discard {n} forks + GC: {} live pages, {} freed ({}), mark+sweep {:.1} ms",
        live,
        freed,
        mib(freed * PAGE as u64),
        gct.as_secs_f64() * 1e3
    );
    main
}

/// Fork-of-fork depth 10: reads at the tip stay flat (no chain walk).
fn chain(cfg: &Cfg, main: &Connection, subjects: u64) {
    let e = engine();
    println!("\n-- fork chain depth 10 --");
    let mut parent = "main".to_owned();
    let mut rng = SmallRng::seed_from_u64(3);
    let mut keep = Vec::new();
    for d in 0..10 {
        let name = format!("chain-{d}");
        let (t, _) = e.fork(&parent, &name);
        let c = open_cow(&name, 2000);
        for k in 0..cfg.commits {
            commit_one(&c, &mut rng, subjects, 500 + (d * 100 + k) as i64);
        }
        if d == 9 {
            let mut r = point_reads(&c, subjects, cfg.reads, 1);
            summary("depth-10 fork point read", &mut r);
            println!("  last fork-of-fork create: {}", fmt_ns(t.as_nanos() as u64));
        }
        keep.push(c);
        parent = name;
    }
    let mut r = point_reads(main, subjects, cfg.reads, 1);
    summary("main point read (same run)", &mut r);
    drop(keep);
    for d in 0..10 {
        e.drop_file(&format!("chain-{d}"));
    }
    e.gc();
}

/// Two forks applying the same commits to the same base share every page.
fn dedupe(cfg: &Cfg, subjects: u64) {
    let e = engine();
    println!("\n-- cross-fork dedupe --");
    let mut grow = Vec::new();
    for name in ["dup-a", "dup-b"] {
        e.fork("main", name);
        let before = e.store.bytes();
        let c = open_cow(name, 2000);
        let mut rng = SmallRng::seed_from_u64(42);
        for k in 0..cfg.commits {
            commit_one(&c, &mut rng, subjects, 900 + k as i64);
        }
        grow.push(e.store.bytes() - before);
    }
    println!("  identical writes: first fork stored {}, second stored {}", mib(grow[0]), mib(grow[1]));
    e.drop_file("dup-a");
    e.drop_file("dup-b");
    e.gc();
}
