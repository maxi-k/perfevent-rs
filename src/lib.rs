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
//! ```
//!
//! Manual Usage:
//! ```rust
//! use perfblock::*;
//! let mut perf = PerfEvent::default();
//! perf.start_counters().expect("error starting counters");
//! let scale = 1000*1000;
//! let mut res = 1;
//! for i in 1..scale {
//!     std::hint::black_box(res += i*res);
//! }
//! perf.stop_counters().expect("error stopping counters");
//! perf.print_columns(scale, true);
//! ```
use std::collections::BTreeMap;
use std::fmt::Write;
use std::io;
use std::time::Instant;

// re-export
#[cfg(unix)]
pub use perfcnt::linux::{
    CacheId, CacheOpId, CacheOpResultId, FileReadFormat, HardwareEventType, PerfCounter, PerfCounterBuilderLinux as Builder,
    SoftwareEventType,
};
pub use perfcnt::AbstractPerfCounter;

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
            ("cycles", Builder::from_hardware_event(HardwareEventType::CPUCycles)),
            (
                "kcycles",
                Builder::from_hardware_event(HardwareEventType::CPUCycles).apply(|b| {
                    b.exclude_user();
                }),
            ),
            ("instructions", Builder::from_hardware_event(HardwareEventType::Instructions)),
            ("L1-misses", Builder::from_cache_event(CacheId::L1D, CacheOpId::Read, CacheOpResultId::Miss)),
            ("LLC-misses", Builder::from_hardware_event(HardwareEventType::CacheMisses)),
            ("branch-misses", Builder::from_hardware_event(HardwareEventType::BranchMisses)),
            ("task-clock", Builder::from_software_event(SoftwareEventType::TaskClock)),
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

    pub fn iter_mut(&mut self) -> std::slice::IterMut<'_, (&'static str, Builder)> {
        self.0.iter_mut()
    }

    pub fn into_iter(self) -> std::vec::IntoIter<(&'static str, Builder)> {
        self.0.into_iter()
    }
}

////////////////////////////////////////////////////////////////////////////////
// PerfEvent

struct CounterState {
    counter: perfcnt::PerfCounter,
    start: FileReadFormat,
    end: FileReadFormat,
}
impl CounterState {
    fn start(&mut self) -> io::Result<()> {
        self.counter.reset()?;
        self.counter.start()?;
        self.start = self.counter.read_fd()?;
        Ok(())
    }

    fn stop(&mut self) -> io::Result<()> {
        self.end = self.counter.read_fd()?;
        self.counter.stop()?;
        Ok(())
    }

    fn read(&self) -> f64 {
        let correction = ((self.end.time_enabled - self.start.time_enabled) as f64)
            / ((self.end.time_running - self.start.time_running) as f64);
        ((self.end.value - self.start.value) as f64) * correction
    }

    fn empty_read_format() -> FileReadFormat {
        FileReadFormat {
            value: 0,
            time_enabled: 0,
            time_running: 0,
            id: 0,
        }
    }
}
impl From<PerfCounter> for CounterState {
    fn from(counter: PerfCounter) -> Self {
        CounterState {
            counter,
            start: Self::empty_read_format(),
            end: Self::empty_read_format(),
        }
    }
}

/// Low-level counter group.
pub struct PerfEvent {
    ctrs: Vec<CounterState>, // in the same order as `names`
    names: Vec<String>,
    t_start: Instant,
    t_stop: Instant,
}

impl Default for PerfEvent {
    /// Construct with the default counter set.
    fn default() -> Self {
        Self::new_or_empty(Events::default())
    }
}

impl PerfEvent {
    /// Construct with the given events; panic if opening any counter fails
    pub fn new(mut events: Events) -> Self {
        Self::try_new(&mut events).expect("Error creating PerfEvent instance")
    }

    /// Construct with the given events; print an error if opening any counter fails, but continue
    pub fn new_or_empty(mut events: Events) -> Self {
        match Self::try_new(&mut events) {
            Ok(ok) => ok,
            Err(err) => {
                eprintln!("error opening counter cycles: {}", err);
                Self {
                    ctrs: Vec::new(),
                    names: Vec::new(),
                    t_start: Instant::now(),
                    t_stop: Instant::now(),
                }
            }
        }
    }

    /// Try to construct with the given events, fail if opening a counter fails
    pub fn try_new(events: &mut Events) -> io::Result<Self> {
        let mut ctrs = Vec::with_capacity(events.len());
        let mut names = Vec::with_capacity(events.len());

        for (name, builder) in events.iter_mut() {
            let c = Self::finalize_builder(builder)?;
            ctrs.push(c);
            names.push((*name).to_owned());
        }
        Ok(Self {
            ctrs,
            names,
            t_start: Instant::now(),
            t_stop: Instant::now(),
        })
    }

    /// Register an additional counter *before* calling `stop_counters`.
    pub fn register_counter<E: Into<Builder>>(&mut self, event: E, name: &str) -> io::Result<()> {
        self.ctrs.push(Self::finalize_builder(&mut event.into())?);
        self.names.push(name.to_owned());
        Ok(())
    }

    /// Start the registered counters
    pub fn start_counters(&mut self) -> io::Result<()> {
        let mut res: Vec<io::Error> = self.ctrs.iter_mut().map(|c| c.start()).filter_map(|c| c.err()).collect();
        match res.pop() {
            Some(err) => {
                self.ctrs.iter_mut().for_each(|c| {
                    let _ = c.stop();
                });
                Err(err)
            }
            None => {
                self.t_start = Instant::now();
                Ok(())
            }
        }
    }

    /// Stop the registered counters
    pub fn stop_counters(&mut self) -> io::Result<()> {
        self.t_stop = Instant::now();
        let mut res: Vec<io::Error> = self.ctrs.iter_mut().map(|c| c.stop()).filter_map(|c| c.err()).collect();
        match res.pop() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }

    /// Counter value by name
    pub fn try_get(&self, name: &str) -> Option<f64> {
        self.names.iter().position(|n| n == name).map(|i| self.counter(i))
    }

    /// Counter value by name, or `NaN` if the name is unknown.
    pub fn get(&self, name: &str) -> f64 {
        self.try_get(name).unwrap_or(f64::NAN)
    }

    /// Derived metrics
    pub fn duration(&self) -> f64 {
        self.t_stop.duration_since(self.t_start).as_secs_f64()
    }
    pub fn duration_us(&self) -> u128 {
        self.t_stop.duration_since(self.t_start).as_nanos()
    }
    pub fn ipc(&self) -> f64 {
        self.get("instructions") / self.get("cycles")
    }
    pub fn cpus(&self) -> f64 {
        self.get("task-clock") / (self.duration_us() as f64)
    }
    pub fn ghz(&self) -> f64 {
        self.get("cycles") / self.get("task-clock")
    }

    /// Finalize a builder spec, converting it into a PerfCounter instance
    fn finalize_builder(b: &mut Builder) -> io::Result<CounterState> {
        let built = b
            .on_cpu(-1) // all cpus
            .for_pid(0) // calling process
            .inherit()
            .disable() // start disabled
            .enable_read_format_time_enabled() // multiplexing counters
            .enable_read_format_time_running()
            .enable_read_format_id()
            .finish()?; // build
        Ok(built.into())
    }

    /// Append CSV columns to `hdr` / `dat` respecting `scale`.
    fn write_columns(&self, scale: u64, mut cols: impl ColumnWriter) {
        for (i, name) in self.names.iter().enumerate() {
            cols.write_f64(name.as_str(), self.counter(i) / scale as f64, 2);
        }
        cols.write_u64(&"scale".to_string(), scale);
        for (name, val) in [("IPC", self.ipc()), ("CPUs", self.cpus()), ("GHz", self.ghz())].into_iter() {
            cols.write_f64(name, val, 2);
        }
    }

    /// Print the CSV columns to stdout
    pub fn print_columns(&self, scale: u64, header: bool) {
        let mut hdr = String::new();
        let mut dat = String::new();
        self.write_columns(scale, (&mut hdr, &mut dat));
        if header {
            println!("{}", hdr);
        }
        println!("{}", dat);
    }

    /// Counter value by index.
    fn counter(&self, idx: usize) -> f64 {
        self.ctrs[idx].read()
    }
}

pub trait ColumnWriter {
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

struct TransposedWriter<'a>(usize, &'a mut String);
impl<'a> ColumnWriter for &mut TransposedWriter<'a> {
    fn write_str(&mut self, name: &str, val: &str) {
        let width = self.0 + 1;
        let _ = write!(self.1, "{:<width$}", name);
        let _ = write!(self.1, ": {}\n", val);
    }
}

////////////////////////////////////////////////////////////////////////////////
// BenchmarkParameters

/// Free-form benchmark meta-data printed as columns.
#[derive(Default, Clone)]
pub struct BenchmarkParameters(BTreeMap<String, String>);
impl BenchmarkParameters {
    pub fn new<S1: Into<String>, S2: Into<String>>(params: impl IntoIterator<Item = (S1, S2)>) -> Self {
        let mut m = Self(BTreeMap::new());
        m.set_all(params);
        m
    }
    pub fn set<K: Into<String>, V: Into<String>>(&mut self, k: K, v: V) {
        self.0.insert(k.into(), v.into());
    }
    pub fn with<K: Into<String>, V: Into<String>>(mut self, k: K, v: V) -> Self {
        self.set(k, v);
        self
    }
    pub fn set_all<S1: Into<String>, S2: Into<String>>(&mut self, params: impl IntoIterator<Item = (S1, S2)>) {
        params.into_iter().for_each(|(k, v)| {
            self.0.insert(k.into(), v.into());
        });
    }
    fn write_columns(&self, mut cols: impl ColumnWriter) {
        for (k, v) in &self.0 {
            cols.write_str(k, v);
        }
    }
}

////////////////////////////////////////////////////////////////////////////////
// PerfEventBlock
pub enum PrintMode {
    Regular(bool), // header
    Transposed,
}
impl Default for PrintMode {
    fn default() -> Self {
        PrintMode::Regular(true)
    }
}

impl From<bool> for PrintMode {
    fn from(value: bool) -> Self {
        PrintMode::Regular(value)
    }
}

/// High-level RAII wrapper - starts on construction, prints on drop.
pub struct PerfEventBlock {
    inner: PerfEvent,
    scale: u64,
    params: BenchmarkParameters,
    print_mode: PrintMode,
}

impl Default for PerfEventBlock {
    /// Create a new PerfEventBlock with scale 1, default events, and no additional columns
    fn default() -> Self {
        Self {
            inner: Default::default(),
            scale: 1,
            params: Default::default(),
            print_mode: Default::default(),
        }
    }
}

impl PerfEventBlock {
    /// Create a new PerfEventBlock instance with custom events, output columns, and scale
    pub fn new(scale: u64, events: Events, params: BenchmarkParameters, print_mode: impl Into<PrintMode>) -> Self {
        let mut res = Self {
            inner: PerfEvent::new_or_empty(events),
            scale,
            params,
            print_mode: print_mode.into(),
        };
        res.inner.start_counters().expect("Error starting counters");
        res
    }

    /// Create a new PerfEventBlock instance with default events, custom output columns, and scale
    pub fn default_events(scale: u64, params: BenchmarkParameters, print_mode: impl Into<PrintMode>) -> Self {
        Self::new(scale, Events::default(), params, print_mode)
    }

    /// Create a new PerfEventBlock instance with default events, default output columns, and custom scale
    pub fn default_params(scale: u64, print_mode: impl Into<PrintMode>) -> Self {
        Self::new(scale, Default::default(), Default::default(), print_mode)
    }

    /// Set a param (output column) on this instance. Useful for adding columns after
    /// the block has been started already.
    pub fn param<S1: Into<String>, S2: Into<String>>(&mut self, k: S1, v: S2) -> &mut Self {
        self.params.set(k, v);
        self
    }

    /// Set multiple parameters
    pub fn params<S1: Into<String>, S2: Into<String>>(&mut self, params: impl IntoIterator<Item = (S1, S2)>) -> &mut Self {
        self.params.set_all(params);
        self
    }

    /// Sets some final parameters (measured externally), then drops, printing statistics
    pub fn finalize_with<S1: Into<String>, S2: Into<String>>(mut self, params: impl IntoIterator<Item = (S1, S2)>) {
        self.params(params);
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

    pub fn print(&self) {
        match self.print_mode {
            PrintMode::Regular(header) => {
                let mut hdr = String::new();
                let mut dat = String::new();
                self.params.write_columns((&mut hdr, &mut dat));
                (&mut hdr, &mut dat).write_f64("time_sec", self.inner.duration(), 6);
                self.inner.write_columns(self.scale, (&mut hdr, &mut dat));
                if header {
                    println!("\n{}", hdr);
                }
                println!("{}", dat);
            }
            PrintMode::Transposed => {
                let mut output = String::new();
                let maxlen = (self.params.0.iter())
                    .map(|item| item.0)
                    .chain(self.inner.names.iter())
                    .fold(0, |acc, item| acc.max(item.len()));
                let mut wr = TransposedWriter(maxlen, &mut output);
                self.params.write_columns(&mut wr);
                (&mut wr).write_f64("time_sec", self.inner.duration(), 6);
                self.inner.write_columns(self.scale, &mut wr);
                println!("{}", output);
            }
        }
    }
}

impl Drop for PerfEventBlock {
    /// Finalize the PerfEventBlock, stopping the counters and printing them
    fn drop(&mut self) {
        if self.inner.stop_counters().is_ok() {
            self.print();
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

    #[test]
    fn sleep_only() {
        let params = BenchmarkParameters::default();
        {
            let _block = PerfEventBlock::default_events(1, params, true);
            sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn long_computation_with_black_box() {
        PerfEventBlock::default_params(1000, true).measure(|p| {
            let mut res = 1;
            for i in 1..(1000 * p.scale()) {
                p.black_box(res += i);
            }
            p.param("res", res.to_string());
        });
    }

    #[test]
    fn custom_events() {
        PerfEventBlock::new(
            1000 * 1000,
            Events::default().add_all([
                ("tlb-miss", Builder::from_cache_event(CacheId::DTLB, CacheOpId::Read, CacheOpResultId::Miss)),
                ("page-faults", Builder::from_software_event(SoftwareEventType::PageFaults)),
            ]),
            BenchmarkParameters::default(),
            true,
        )
        .measure(|p| {
            let mut res = 1;
            for i in 1..p.scale() {
                p.black_box(res += i);
            }
            p.param("res", res.to_string());
        });
    }

    #[test]
    fn manual_usage() {
        let mut perf = PerfEvent::default();
        perf.start_counters().expect("error starting counters");
        let scale = 1000 * 1000 * 1000;
        let mut res = 1;
        for i in 1..scale {
            std::hint::black_box(res += i);
        }
        perf.stop_counters().expect("error stopping counters");
        perf.print_columns(scale, true);
    }
}
