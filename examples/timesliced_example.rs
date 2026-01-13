use std::time::Duration;

use perf_event_block::{BenchmarkParameters, PerfEventTimesliced};

fn main() {
    let scale = 1000;
    // <- ... init code we don't want to measure
    let mut res: i32 = 0;
    {
        // <-- measure this block, with time-sliced sampling every 100µs
        let mut _perf =
            PerfEventTimesliced::default_events(scale, BenchmarkParameters::default(), Duration::from_millis(500), true);
        for j in 0i32..10 {
            for i in 0i32..100000 {
                res += i.isqrt();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    } // <-- _perf is dropped here, printing the summary and time-series CSV
    println!("result {}", res)
}
