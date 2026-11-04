//! Best Match timings: cargo run --release --example rank -- [ROOT...] -- QUERY...
//! Per query: name sort (first 500), Best Match (first 500), and Best Match narrowed from the
//! query minus its last letter (what typing that letter costs). Median of 9 runs.
use kestrel_index::{Extra, Index, Query, Sort, scan};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

fn median(mut f: impl FnMut() -> usize) -> (Duration, usize) {
    let mut t: Vec<_> = (0..9)
        .map(|_| {
            let s = Instant::now();
            let n = f();
            (s.elapsed(), n)
        })
        .collect();
    t.sort();
    t[4]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let p = args.iter().position(|a| a == "--").expect("ROOT... -- QUERY...");
    let mut ix = Index::default();
    for r in &args[..p] {
        scan::scan(&mut ix, r.as_ref(), &[]).expect("scan");
    }
    eprintln!(
        "{} entries\n{:<14} {:>9} {:>10} {:>12} {:>12}",
        ix.len(),
        "query",
        "hits",
        "name",
        "best match",
        "narrowed"
    );
    let no = AtomicBool::new(false);
    for q in &args[p + 1..] {
        let query = Query::parse(q).unwrap();
        let (tn, n) = median(|| ix.search_top(&query, Sort::Name, false, None, Some(500), &no).0.len());
        let (tr, _) = median(|| ix.search_top(&query, Sort::Relevance, false, None, Some(500), &no).0.len());
        let prev = &q[..q.len() - 1];
        let tw = if kestrel_index::query::narrows(prev, q) {
            let within = ix.search(&Query::parse(prev).unwrap(), Sort::Name, false, None);
            let extra = Extra {
                within: Some(within),
                ..Default::default()
            };
            format!(
                "{:.1?}",
                median(|| ix.search_with(&query, Sort::Relevance, false, None, Some(500), &no, &extra).0.len()).0
            )
        } else {
            "-".into()
        };
        eprintln!("{q:<14} {n:>9} {:>10.1?} {:>12.1?} {tw:>12}", tn, tr);
        let top = ix.search_top(&query, Sort::Relevance, false, None, Some(500), &no).0;
        for &i in top.iter().take(3) {
            eprintln!("    {}", String::from_utf8_lossy(&ix.path(i)));
        }
    }
}
