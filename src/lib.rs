//! Rust rewrite of the venerable <https://github.com/viktorleis/perfevent>
//! with some rust-specific niceties and improvements.
//!
//! Usage example (using the C++-like RAII API):
//! ```rust
//! use perfblock::*;
//! use std::thread::sleep;
//! use std::time::Duration;
//!
//! let params = BenchmarkParameters::default(); // additional output columns
//! {
//!   let perf = PerfEventBlock::default_events(/*scale*/1, params, /*print header*/true);
//!   sleep(Duration::from_millis(10));
//! } // <- `perf` dropped here, counters stopped, statistics printed
//! ```
//!
//! Lambda-style API:
//! ```rust
//! use perfblock::*;
//! PerfEventBlock::default_params(1000, true).measure(|p: &mut PerfEventBlock| {
//!     let mut res = 1;
//!     for i in 1..(1000*p.scale()) {
//!         // std::hint::black_box re-exported for convenience
//!         p.black_box(res += i);
//!     }
//!     p.param("res", res.to_string());
//! });
//! ```
//!
//! Custom Events:
//! ```rust
//! use perfblock::*;
//! PerfEventBlock::new(1000, Events::default().add_all([
//!     ("tlb-miss", Builder::from_cache_event(CacheId::DTLB, CacheOpId::Read, CacheOpResultId::Miss)),
//!     ("page-faults", Builder::from_software_event(SoftwareEventType::PageFaults)),
//! ]), BenchmarkParameters::default(), true).measure(|p| {
//!     // long computation
//! });
//!
//! ```
// re-export
pub use perfcnt::AbstractPerfCounter;
pub use perfcnt::linux::{
    CacheId, CacheOpId, CacheOpResultId, HardwareEventType, SoftwareEventType,
    PerfCounterBuilderLinux as Builder
};

use std::collections::BTreeMap;
use std::fmt::Write;
use std::io;
use std::time::Instant;

////////////////////////////////////////////////////////////////////////////////
// Linux Event Counters 

trait BuilderExt: Sized {
    fn apply<F: FnOnce(&mut Self)>(mut self, f: F) -> Self {
        f(&mut self);
        self
    }
}
impl BuilderExt for Builder {}

pub struct Events(Vec<(&'static str, Builder)>);
impl Default for Events {
    fn default() -> Self {
        Self(vec![
            ("cycles",            Builder::from_hardware_event(HardwareEventType::CPUCycles)),
            ("kcycles",           Builder::from_hardware_event(HardwareEventType::CPUCycles).apply(|b| { b.exclude_user(); })),
            ("instructions",      Builder::from_hardware_event(HardwareEventType::Instructions)),
            ("L1-misses",         Builder::from_cache_event(CacheId::L1D, CacheOpId::Read, CacheOpResultId::Miss)),
            ("LLC-misses",        Builder::from_hardware_event(HardwareEventType::CacheMisses)),
            ("branch-misses",     Builder::from_hardware_event(HardwareEventType::BranchMisses)),
            ("task-clock",        Builder::from_software_event(SoftwareEventType::TaskClock)),
        ])
    }
}

impl Events {
    pub fn add(mut self, name: &'static str, ev: Builder) -> Self {
        self.0.push((name, ev));
        self
    }

    pub fn add_all(mut self, evts: impl IntoIterator<Item = (&'static str, Builder)>) -> Self {
        self.0.extend(evts);
        self
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (&'static str, Builder)> {
        self.0.iter()
    }

    pub fn into_iter(self) -> std::vec::IntoIter<(&'static str, Builder)> {
        self.0.into_iter()
    }
}

////////////////////////////////////////////////////////////////////////////////
// PerfEvent 

/// Low-level counter group.
pub struct PerfEvent {
    ctrs:    Vec<perfcnt::PerfCounter>, // in the same order as `names`
    names:   Vec<String>,
    counts:  Vec<u64>,                  // filled by `stop_counters`
    t_start: Instant,
    t_stop: Instant,
}

impl Default for PerfEvent {
    /// Construct with the default counter set.
    fn default() -> Self {
        Self::new(Events::default())
    }
}

impl PerfEvent {
    /// Construct with the given events; panic if opening any counter fails
    pub fn new(events: Events) -> Self {
        Self::try_new(events).expect("Error creating PerfEvent instance")
    }

    /// Try to construct with the given events, fail if opening a counter fails
    pub fn try_new(events: Events) -> io::Result<Self> {
        let mut ctrs  = Vec::with_capacity(events.len());
        let mut names = Vec::with_capacity(events.len());

        for (name, builder) in events.into_iter() {
            let c = Self::finalize_builder(builder);
            ctrs.push(c);
            names.push((*name).to_owned());
        }
        let len = ctrs.len();

        Ok(Self { ctrs, names, counts: vec![0; len], t_start: Instant::now(), t_stop: Instant::now() })
    }

    /// Register an additional counter *before* calling `stop_counters`.
    pub fn register_counter<E: Into<Builder>>(&mut self, event: E, name: &str) -> io::Result<()> {
        self.ctrs.push(Self::finalize_builder(event.into()));
        self.names.push(name.to_owned());
        self.counts.push(0);
        Ok(())
    }

    /// Start the registered counters
    pub fn start_counters(&mut self) -> io::Result<()> {
        let res = self.ctrs.iter().map(|c| { c.reset()?; c.start() }).find(|c| c.is_err());
        if let Some(err) = res {
            self.ctrs.iter().for_each(|c| { let _ = c.stop(); });
            err
        } else {
            self.t_start = Instant::now();
            Ok(())
        }
    }

    /// Stop the registered counters
    pub fn stop_counters(&mut self) -> io::Result<()> {
        self.t_stop = Instant::now();
        match self.ctrs.iter_mut().enumerate().map(|(idx, c)| {
            self.counts[idx] = c.read()?;
            c.stop()?;
            Ok(())
        }).find(|c| c.is_err()) {
            Some(err) => err,
            None => Ok(())
        }
    }

    /// Counter value by name
    pub fn try_get(&self, name: &str) -> Option<f64> {
        self.names.iter().position(|n| n == name).map(|i| self.counter(i))
    }

    /// Counter value by name, or `-1` if the name is unknown.
    pub fn get(&self, name: &str) -> f64 {
        self.try_get(name).unwrap_or(-1.0)
    }

    /// Derived metrics
    pub fn duration(&self) -> f64 { self.t_stop.duration_since(self.t_start).as_secs_f64() }
    pub fn duration_us(&self) -> u128 { self.t_stop.duration_since(self.t_start).as_micros() }
    pub fn ipc(&self) -> f64 { self.get("instructions") / self.get("cycles") }
    pub fn cpus(&self) -> f64 { self.get("task-clock") / (self.duration_us() as f64) }
    pub fn ghz(&self) -> f64 { self.get("cycles") / self.get("task-clock") }

    /// Finalize a builder spec, converting it into a PerfCounter instance
    fn finalize_builder(mut b: Builder) -> perfcnt::PerfCounter {
        b.on_cpu(-1) // all cpus
         .for_pid(0) // calling process
         .inherit()
         .disable()  // start disabled
         .finish()   // build
         .expect("Error opening counter")
    }

    /// Counter value by index.
    // TODO Multiplexing correction, see perfevent.hpp:{58,108}
    fn counter(&self, idx: usize) -> f64 { self.counts[idx] as f64 }

    /// Append CSV columns to `hdr` / `dat` respecting `scale`.
    fn write_columns(&self, scale: u64, hdr: &mut String, dat: &mut String) {
        let mut cols = (hdr, dat);
        for (i, name) in self.names.iter().enumerate() {
            cols.write_f64(name.as_str(), self.counter(i)/scale as f64, 2);
        }
        cols.write_u64(&"scale".to_string(), scale);
        for (name, val) in [("IPC", self.ipc()), ("CPUs", self.cpus()), ("GHz", self.ghz())].into_iter() {
            cols.write_f64(name, val, 2);
        }
    }
}

trait ColumnWriter {
    fn write_str(&mut self, name: &str, val: &str);

    fn write_u64(&mut self, name: &str, val: u64) {
        self.write_str(name, val.to_string().as_str());
    }
    fn write_f64(&mut self, name: &str, val: f64, decimals: usize) {
        self.write_str(name, &format!("{:.decimals$}", val));
    }
}

impl ColumnWriter for (&mut String, &mut String) {
    fn write_str(&mut self, name: &str, val: &str) {
        let width = std::cmp::max(name.len(), val.len());
        let _ = write!(self.0, "{:>width$}, ", name);
        let _ = write!(self.1, "{:>width$}, ", val);
    }
}

////////////////////////////////////////////////////////////////////////////////
// BenchmarkParameters 

/// Free-form benchmark meta-data printed as columns.
#[derive(Default, Clone)]
pub struct BenchmarkParameters(BTreeMap<String, String>);
impl BenchmarkParameters {
    pub fn new<S1: Into<String>, S2: Into<String>>(params: impl IntoIterator<Item = (S1, S2)>) -> Self {
        let mut m = BTreeMap::new();
        params.into_iter().for_each(|(k, v)| { m.insert(k.into(), v.into()); });
        Self(m)
    }
    pub fn set<K: Into<String>, V: Into<String>>(&mut self, k: K, v: V) { self.0.insert(k.into(), v.into()); }
    pub fn with<K: Into<String>, V: Into<String>>(&mut self, k: K, v: V) -> &mut Self {
        self.set(k, v);
        self
    }
    fn write_columns(&self, hdr: &mut String, dat: &mut String) {
        let mut cols = (hdr, dat);
        for (k, v) in &self.0 {
            cols.write_str(k, v);
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// PerfEventBlock 

/// High-level RAII wrapper - starts on construction, prints on drop.
pub struct PerfEventBlock {
    inner: PerfEvent,
    scale: u64,
    params: BenchmarkParameters,
    print_header: bool,
}

impl Default for PerfEventBlock {
    /// Create a new PerfEventBlock with scale 1, default events, and no additional columns
    fn default() -> Self {
        Self { inner: Default::default(), scale: 1, params: Default::default(), print_header: true }
    }
}

impl PerfEventBlock {

    /// Create a new PerfEventBlock instance with custom events, output columns, and scale
    pub fn new(scale: u64, events: Events, params: BenchmarkParameters, print_header: bool) -> Self {
        let mut res = Self { inner: PerfEvent::new(events), scale, params, print_header };
        res.inner.start_counters().expect("Error starting counters");
        res
    }

    /// Create a new PerfEventBlock instance with default events, custom output columns, and scale
    pub fn default_events(scale: u64, params: BenchmarkParameters, print_header: bool) -> Self {
        Self::new(scale, Events::default(), params, print_header)
    }

    /// Create a new PerfEventBlock instance with default events, default output columns, and custom scale
    pub fn default_params(scale: u64, print_header: bool) -> Self {
        Self::new(scale, Default::default(), Default::default(), print_header)
    }

    /// Set a param (output column) on this instance. Useful for adding columns after
    /// the block has been started already.
    pub fn param<S1: Into<String>, S2: Into<String>>(&mut self, k: S1, v: S2) -> &mut Self {
        self.params.set(k, v);
        self
    }

    /// Get the associated PerfEvent instance
    pub fn instance(&mut self) -> &mut PerfEvent {
       &mut self.inner
    }

    /// Get the counter with the given name. Note that this
    /// will only return proper results after this instance
    /// has been finalized.
    pub fn counter(&self, name: &str) -> f64 {
        self.inner.get(name)
    }

    /// Get the configured scale
    pub fn scale(&self) -> u64 {
       self.scale
    }

    /// Convenience function for measuring the content of the passed lambda
    pub fn measure<T, F: FnOnce(&mut Self) -> T>(mut self, comp: F) -> T {
        comp(&mut self)
        // self dropped here
    }

    /// Convenience access to std::hint::black_box
    #[inline]
    pub fn black_box<T>(&self, dummy: T) -> T {
        std::hint::black_box(dummy)
    }
}

impl Drop for PerfEventBlock {
    /// Finalize the PerfEventBlock, stopping the counters and printing them
    fn drop(&mut self) {
        if self.inner.stop_counters().is_ok() {
            let mut hdr = String::new();
            let mut dat = String::new();
            self.params.write_columns(&mut hdr, &mut dat);
            (&mut hdr, &mut dat).write_f64("time sec", self.inner.duration(), 6);

            self.inner.write_columns(self.scale, &mut hdr, &mut dat);
            if self.print_header { println!("{}", hdr); }
            println!("{}", dat);
        } else {
            eprintln!("Error stopping counters");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    // #[test]
    // fn sleep_only() {
    //     let params = BenchmarkParameters::default();
    //     {
    //         let _block = PerfEventBlock::default_events(1, params, true);
    //         sleep(Duration::from_millis(10));
    //     }
    // }

    // #[test]
    // fn long_computation_with_black_box() {
    //     PerfEventBlock::default_params(1000, true).measure(|p| {
    //         let mut res = 1;
    //         for i in 1..(1000*p.scale()) {
    //             p.black_box(res += i);
    //         }
    //         p.param("res", res.to_string());
    //     });
    // }

    #[test]
    fn custom_events() {
        PerfEventBlock::new(1000, Events::default().add_all([
            ("tlb-miss", Builder::from_cache_event(CacheId::DTLB, CacheOpId::Read, CacheOpResultId::Miss)),
            ("page-faults", Builder::from_software_event(SoftwareEventType::PageFaults)),
        ]), BenchmarkParameters::default(), true).measure(|p| {
            let mut res = 1;
            for i in 1..(1000*p.scale()) {
                p.black_box(res += i);
            }
            p.param("res", res.to_string());
        });
    }
}
