use std::fmt::Write;

pub enum PrintMode {
    Regular(bool), // header
    Transposed,
    Disabled,
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

pub(crate) struct TransposedWriter<'a>(pub(crate) usize, pub(crate) &'a mut String);
impl<'a> ColumnWriter for &mut TransposedWriter<'a> {
    fn write_str(&mut self, name: &str, val: &str) {
        let width = self.0 + 1;
        let _ = write!(self.1, "{:<width$}", name);
        let _ = write!(self.1, ": {}\n", val);
    }
}
