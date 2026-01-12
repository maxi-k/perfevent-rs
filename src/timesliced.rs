use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::{BenchmarkParameters, Events, PerfEvent};

/// Message type for live benchmark-parameter updates.
///
/// Implementations should update (merge/overwrite) values in the provided
/// [`BenchmarkParameters`].
pub trait BenchmarkParameterUpdates: Send + 'static {
    fn apply(self, params: &mut BenchmarkParameters, scale: &mut u64);

    fn apply_all<I>(params: &mut BenchmarkParameters, scale: &mut u64, iter: I) where Self: Sized, I: Iterator<Item = Self> {
        for update in iter {
            update.apply(params, scale);
        }
    }
}

impl BenchmarkParameterUpdates for BenchmarkParameters {
    fn apply(self, params: &mut BenchmarkParameters, _scale: &mut u64) {
        params.set_all(self.0);
    }
}

/// RAII wrapper that periodically samples a [`PerfEvent`] on a background thread
/// and prints the *delta* since the previous sample.
///
/// This is intended for a live view while a computation runs.
///
/// Notes:
/// - The underlying counters are started on construction.
/// - Samples are printed as CSV lines to stdout.
/// - On drop, the sampler thread is stopped and joined.
///
/// # Example
/// ```rust
/// use perf_event_block::*;
/// use std::thread::sleep;
/// use std::time::Duration;
///
/// let _ts = PerfEventTimesliced::default_events(1, Duration::from_millis(1), /*header*/ true);
/// sleep(Duration::from_millis(25));
/// ```
pub struct PerfEventTimesliced<Update: BenchmarkParameterUpdates = BenchmarkParameters> {
    stop: Arc<AtomicBool>,
    tx: Sender<Update>,
    handle: Option<JoinHandle<()>>,
}

impl<Update: BenchmarkParameterUpdates> PerfEventTimesliced<Update> {
    /// Create a new instance with custom events.
    pub fn new(scale: u64, events: Events, init: Update, period: Duration, header: bool) -> Self {
        let mut perf = PerfEvent::new_or_empty(events);
        perf.start_counters().expect("error starting counters");

        let period = period.max(Duration::from_micros(1));
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);

        let (tx, rx) = mpsc::channel::<Update>();

        let handle = thread::spawn(move || sampler_loop(perf, scale, init, period, header, stop2, rx));

        Self {
            stop,
            tx,
            handle: Some(handle),
        }
    }

    /// Create a new instance with default events.
    pub fn default_events(scale: u64, init: Update, period: Duration, header: bool) -> Self {
        Self::new(scale, Events::default(), init, period, header)
    }

    /// Send a benchmark-parameter update to the sampler thread.
    ///
    /// The update will be applied before the next printed sample.
    pub fn update(&self, upd: Update) -> Result<(), mpsc::SendError<Update>> {
        self.tx.send(upd)
    }

    /// Get a clone of the update sender.
    pub fn sender(&self) -> Sender<Update> {
        self.tx.clone()
    }

    /// Stop the sampler thread and wait for it.
    pub fn stop(mut self) {
        self.stop_thread();
    }

    fn stop_thread(&mut self) {
        if let Some(h) = self.handle.take() {
            self.stop.store(true, Ordering::Relaxed);
            let _ = h.join();
        }
    }
}

impl<Update: BenchmarkParameterUpdates> Drop for PerfEventTimesliced<Update> {
    fn drop(&mut self) {
        self.stop_thread();
    }
}

fn sampler_loop<Update: BenchmarkParameterUpdates>(
    mut perf: PerfEvent,
    mut scale: u64,
    init: Update,
    period: Duration,
    header: bool,
    stop: Arc<AtomicBool>,
    rx: Receiver<Update>,
) {
    let mut params = BenchmarkParameters::default();
    init.apply(&mut params, &mut scale);

    let mut last_param_len = params.0.len();

    // Ensure these always exist so we can print them as first columns.
    params.set("duration_ms", "");
    params.set("timestamp_ms", "");

    let t0 = Instant::now();
    let mut last_t = t0;

    let mut dat = String::new();
    let mut hdr = String::new();
    while !stop.load(Ordering::Relaxed) {
        dat.clear();
        hdr.clear();
        thread::sleep(period);

        // Drain parameter updates.
        Update::apply_all(&mut params, &mut scale, rx.try_iter());

        let now = Instant::now();
        let slice_duration = now.duration_since(last_t);
        let duration_ms = slice_duration.as_millis() as u64;
        let timestamp_ms = now.duration_since(t0).as_millis() as u64;
        last_t = now;

        if let Err(e) = perf.fetch_current_counters() {
            eprintln!("perfevent: error reading counters: {e}");
            break;
        }

        params.set("duration_ms", duration_ms.to_string());
        params.set("timestamp_ms", timestamp_ms.to_string());

        params.write_columns((&mut hdr, &mut dat));

        // Use PerfEvent's built-in derived metrics + formatting.
        // Since we operate on a slice window (start..end) these counters represent
        // *deltas* for the slice.
        perf.write_columns(scale, (&mut hdr, &mut dat));

        // If parameter keys changed, re-print the header (cheap heuristic).
        let param_len = params.0.len();
        if header && param_len != last_param_len {
            last_param_len = param_len;
            println!("{}", hdr);
        }
        println!("{}", dat);

        // Reset the slice window for the next iteration.
        perf.advance_counter_start_to_end();
    }

    let _ = perf.stop_counters();
}
