//! cargo run --release --example scan -- [ROOT...] -- QUERY...
use kestrel_index::{Index, Query, Sort, scan};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (roots, queries) = match args.iter().position(|a| a == "--") {
        Some(p) => (args[..p].iter().map(Into::into).collect(), args[p + 1..].to_vec()),
        None => (scan::local_mounts(&[]), args),
    };
    let mut ix = Index::default();
    let t = Instant::now();
    for r in &roots {
        let s = scan::scan(&mut ix, r, &scan::load_excludes("/etc/kestrel/excludes".as_ref())).expect("scan");
        eprintln!("{}: {} entries, {} errors", r.display(), s.entries, s.errors);
    }
    eprintln!("scanned {} entries in {:.2?}, ~{} MB", ix.len(), t.elapsed(), ix.approx_bytes() >> 20);
    for q in &queries {
        let t = Instant::now();
        let hits = ix.search(&Query::parse(q).expect("query"), Sort::Name, false, None);
        eprintln!("\n{q:?}: {} hits in {:.2?}", hits.len(), t.elapsed());
        for &i in hits.iter().take(5) {
            println!("  {}", String::from_utf8_lossy(&ix.path(i)));
        }
    }
}
