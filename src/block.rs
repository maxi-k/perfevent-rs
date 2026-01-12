use crate::{BenchmarkParameters, ColumnWriter, PerfEvent, PrintMode};

/// High-level RAII wrapper - starts on construction, prints on drop.
pub struct PerfEventBlock {
    pub(crate) inner: PerfEvent,
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
    pub fn new(scale: u64, events: crate::Events, params: BenchmarkParameters, print_mode: impl Into<PrintMode>) -> Self {
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
        Self::new(scale, crate::Events::default(), params, print_mode)
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

    pub fn disable_print(&mut self) {
        self.print_mode = PrintMode::Disabled;
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
                    .fold(0, |acc, item| acc.max(item.len()));
                let mut wr = crate::print::TransposedWriter(maxlen, &mut output);
                self.params.write_columns(&mut wr);
                (&mut wr).write_f64("time_sec", self.inner.duration(), 6);
                self.inner.write_columns(self.scale, &mut wr);
                println!("{}", output);
            }
            PrintMode::Disabled => {}
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
