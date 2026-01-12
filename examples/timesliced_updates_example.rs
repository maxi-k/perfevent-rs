use std::thread::sleep;
use std::time::Duration;

use perf_event_block::{BenchmarkParameters, PerfEventTimesliced};

fn main() {
    let scale = 1000;

    // Measure this block, with time-sliced sampling.
    let perf = PerfEventTimesliced::default_events(
        scale,
        BenchmarkParameters::default().with("phase", "init"),
        Duration::from_millis(250),
        true,
    );

    // Simulate work in repeatedly labeled phases.
    for (phase, iters) in [("warmup", 50_000), ("compute", 200_000), ("cooldown", 50_000)] {
        perf.update(BenchmarkParameters::default().with("phase", phase))
            .expect("sampler thread still alive");

        for _i in 0..2 {
            let mut acc: u64 = 0;
            for i in 0..iters {
                acc = acc.wrapping_add((i as u64).wrapping_mul(31));
                std::hint::black_box(acc);
            }

            // Ensure we see a couple of lines for each phase.
            sleep(Duration::from_millis(250));
        }
    }

    // A final update so the last printed line includes it.
    perf.update(BenchmarkParameters::default().with("done", "true"))
        .ok();

    // Dropping `perf` stops the sampler thread.
}
