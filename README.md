Single‑file RAII wrapper replicating <https://github.com/viktorleis/perfevent>.

Usage example (similar to the C++ API):
```rust
let mut params = BenchmarkParameters::default();
params.set("query", "Q1");
{ // scope measured
  let _blk = PerfEventBlock::new(/*scale=*/1, params, /*print_header=*/true)?;
  //   // do some work
}
```
