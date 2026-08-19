fn main() {
    let mut by_bundle = std::collections::BTreeMap::new();
    for op in weaver_api::operations() {
        *by_bundle.entry(op.bundle).or_insert(0) += 1;
    }
    let total: i32 = by_bundle.values().sum();
    for (b, n) in &by_bundle {
        println!("  {b:12} {n}");
    }
    println!("  {:12} {total}", "TOTAL");
}
