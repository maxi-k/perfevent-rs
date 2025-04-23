//! Single-file RAII wrapper replicating https://github.com/viktorleis/perfevent
//! Requires `perf-event` >= 0.4 (tested with 0.4.8).
//!
//! Usage example (similar to the C++ API):
//! ```ignore
//! let mut params = BenchmarkParameters::default();
//! params.set("query", "Q1");
//! { // scope measured
//!   let _blk = PerfEventBlock::new(/*scale=*/1, params, /*print_header=*/true)?;
//!   //   // do some work
//! }
//! ```
//! // when `_blk` goes out of scope, statistics are printed automatically

use std::collections::BTreeMap;
use std::fmt::Write; // for `write!` on `String`
use std::io;
use std::time::Instant;

use perf_event::events::{self, Cache, CacheOp, CacheResult, Hardware, Software, WhichCache};
use perf_event::{Builder, Counter, Group};

const DEFAULT_EVENTS: &[(&str, events::Event)] = &[
    ("cycles",            events::Event::Hardware(Hardware::CPU_CYCLES)),
    ("instructions",      events::Event::Hardware(Hardware::INSTRUCTIONS)),
    ("L1-misses",         events::Event::Cache(Cache { which: WhichCache::L1D, operation: CacheOp::READ, result: CacheResult::MISS  })),
    ("LLC-misses",        events::Event::Hardware(Hardware::CACHE_MISSES)),
    ("branch-misses",     events::Event::Hardware(Hardware::BRANCH_MISSES)),
    ("task-clock",        events::Event::Software(Software::TASK_CLOCK)),
];

/// Low-level counter group.
pub struct PerfEvent {
    group:   Group,
    ctrs:    Vec<Counter>,   // in the same order as `names`
    names:   Vec<String>,
    counts:  Vec<u64>,       // filled by `stop_counters`
    t_start: Instant,
}

impl PerfEvent {
    /// Construct with the default counter set and enable immediately.
    pub fn new() -> io::Result<Self> {
        let mut group = Group::new()?;
        let mut ctrs  = Vec::with_capacity(DEFAULT_EVENTS.len());
        let mut names = Vec::with_capacity(DEFAULT_EVENTS.len());

        for (name, ev) in DEFAULT_EVENTS {
            let c = Builder::new().group(&mut group).kind(ev.clone()).build()?;
            ctrs.push(c);
            names.push((*name).to_owned());
        }
        let len = ctrs.len();

        Ok(Self { group, ctrs, names, counts: vec![0; len], t_start: Instant::now() })
    }

    /// Register an additional counter *before* calling `stop_counters`.
    pub fn register_counter<E>(&mut self, event: E, name: &str) -> io::Result<()>
    where
        E: Into<events::Event>,
    {
        let c = Builder::new().group(&mut self.group).kind(event.into()).build()?;
        self.ctrs.push(c);
        self.names.push(name.to_owned());
        self.counts.push(0);
        Ok(())
    }

    pub fn start_counters(&mut self) -> io::Result<()> {
        self.group.enable()?;
        Ok(())
    }

    /// Disable the group and record the counts.
    pub fn stop_counters(&mut self) -> io::Result<()> {
        self.group.disable()?;
        let snapshot = self.group.read()?; // `Counts`
        for (i, ctr) in self.ctrs.iter().enumerate() {
            self.counts[i] = snapshot[ctr];
        }
        Ok(())
    }

    /// Counter value by index.
    pub fn counter(&self, idx: usize) -> f64 { self.counts[idx] as f64 }

    /// Counter value by name, or `-1` if the name is unknown.
    pub fn get(&self, name: &str) -> f64 {
        self.names.iter().position(|n| n == name).map(|i| self.counter(i)).unwrap_or(-1.0)
    }

    /// Derived metrics
    pub fn duration(&self) -> f64 { self.t_start.elapsed().as_secs_f64() }
    pub fn ipc(&self) -> f64 { self.get("instructions") / self.get("cycles") }
    pub fn cpus(&self) -> f64 { self.get("task-clock") / (self.duration() * 1e9) }
    pub fn ghz(&self) -> f64 { self.get("cycles") / self.get("task-clock") }

    /// Append CSV columns to `hdr` / `dat` respecting `scale`.
    fn write_columns(&self, scale: u64, hdr: &mut String, dat: &mut String) {
        for (i, name) in self.names.iter().enumerate() {
            let _ = write!(hdr, "{}, ", name);
            let _ = write!(dat, "{:.2}, ", self.counter(i) / scale as f64);
        }
        let _ = write!(hdr, "scale, IPC, CPUs, GHz");
        let _ = write!(dat, "{}, {:.2}, {:.2}, {:.2}", scale, self.ipc(), self.cpus(), self.ghz());
    }
}

/// Free-form benchmark meta-data printed as columns.
#[derive(Default, Clone)]
pub struct BenchmarkParameters(BTreeMap<String, String>);
impl BenchmarkParameters {
    pub fn set<K: Into<String>, V: Into<String>>(&mut self, k: K, v: V) { self.0.insert(k.into(), v.into()); }
    fn write_columns(&self, hdr: &mut String, dat: &mut String) {
        for (k, v) in &self.0 {
            let _ = write!(hdr, "{}, ", k);
            let _ = write!(dat, "{}, ", v);
        }
    }
}

/// High-level RAII wrapper - starts on construction, prints on drop.
pub struct PerfEventBlock {
    inner: PerfEvent,
    scale: u64,
    params: BenchmarkParameters,
    print_header: bool,
}

impl PerfEventBlock {
    pub fn new(scale: u64, params: BenchmarkParameters, print_header: bool) -> io::Result<Self> {
        let mut res = Self { inner: PerfEvent::new()?, scale, params, print_header };
        res.inner.start_counters()?;
        Ok(res)
    }
}

impl Drop for PerfEventBlock {
    fn drop(&mut self) {
        if self.inner.stop_counters().is_ok() {
            let mut hdr = String::new();
            let mut dat = String::new();

            self.params.write_columns(&mut hdr, &mut dat);
            let _ = write!(hdr, "time sec, ");
            let _ = write!(dat, "{:.6}, ", self.inner.duration());

            self.inner.write_columns(self.scale, &mut hdr, &mut dat);
            if self.print_header { println!("{}", hdr); }
            println!("{}", dat);
        } else {
            eprintln!("Error stopping counters");
        }
    }
}

// Minimal test
#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn smoke() {
        let params = BenchmarkParameters::default();
        {
            let _block = PerfEventBlock::new(1, params, true).unwrap();
            sleep(Duration::from_millis(10));
        }
    }
}
