use std::io::Write;

fn main() {
    let code = h5000m_fancontrol::cli::run(std::env::args().skip(1));
    // The status output is written with print!, which buffers; flushing here
    // keeps `h5000m-fancontrol status | ...` usable from procd and LuCI rpcd.
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}
