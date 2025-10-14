use perf_event_block::PerfEventBlock;

fn main() {
    let scale = 1000;
    // <- ... init code we don't want to measure
    let mut res: i32 = 0;
    {
        // <-- measure this block
        let mut _perf = PerfEventBlock::default_params(scale, true);
        for i in 0i32..1000 {
            res += i.isqrt();
        }
    } // <-- _perf is dropped here, printing the perf counters
    println!("result {}", res)
}
