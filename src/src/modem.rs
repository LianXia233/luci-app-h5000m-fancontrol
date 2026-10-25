//! MT5700M modem temperature, bridged from the at-webserver manager.
//!
//! The 5G modem manager (`luci-app-mt5700`) runs a local RPC service on the
//! loopback interface whose `at` method relays a single AT command to the
//! modem. The controller asks it for `AT^CHIPTEMP?` at a throttled pace,
//! reduces the twelve die readings to their hottest valid value and stores it
//! in the module cache file the sensor layer already consumes. The manager
//! stays the only owner of the serial port, the control loop never talks to
//! the modem directly, and an absent manager simply leaves the module sensor
//! reported as missing.

use crate::paths::Paths;
use crate::platform;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::Duration;

/// The manager answers one JSON line per request and a wedged backend must
/// never stall the control loop behind it.
const RPC_TIMEOUT: Duration = Duration::from_millis(1500);

/// One chip temperature query at most per interval: the AT channel is shared
/// with the manager's own pages and the die temperatures move slowly.
pub const REFRESH_SECS: u64 = 30;

const RPC_REQUEST: &str = "{\"id\":1,\"method\":\"at\",\"params\":{\"cmd\":\"AT^CHIPTEMP?\"}}";
/// The modem reports failed sensors as 65535 in 0.1 °C units, the same
/// sentinel the manager's own frontend parser drops.
const CHIPTEMP_FAIL_RAW: i32 = 65535;
const CHIPTEMP_RAW_MIN: i32 = crate::sensor::TEMP_MIN_C * 10;
const CHIPTEMP_RAW_MAX: i32 = crate::sensor::TEMP_MAX_C * 10;

/// Rate-limited module cache refresher held by the daemon loop.
#[derive(Default)]
pub struct Refresher {
    last_query: Option<u64>,
}

impl Refresher {
    pub fn refresh(&mut self, paths: &Paths, now: u64) {
        self.refresh_with(paths, now, query_and_store);
    }

    /// Testable core: `query` fetches and stores a reading, returning the
    /// temperature it cached if the manager answered.
    pub fn refresh_with(
        &mut self,
        paths: &Paths,
        now: u64,
        mut query: impl FnMut(&Paths, u64) -> Option<f64>,
    ) {
        if paths.mt5700_rpc_port == 0 {
            return;
        }
        if self
            .last_query
            .is_some_and(|previous| now.saturating_sub(previous) < REFRESH_SECS)
        {
            return;
        }
        self.last_query = Some(now);
        query(paths, now);
    }
}

/// Ask the manager for the chip temperatures and store the hottest one.
/// Returns the stored temperature; `None` covers every "not available this
/// cycle" case, which the sensor layer expresses as a missing cache.
pub fn query_and_store(paths: &Paths, now: u64) -> Option<f64> {
    if paths.mt5700_rpc_port == 0 {
        return None;
    }
    let data = rpc_chip_temp(paths.mt5700_rpc_port)?;
    let temperature = chip_temp_max(&data)?;
    store_cache(&paths.module_cache, now, temperature)?;
    Some(temperature)
}

/// Send one AT request over the manager's line protocol and return the `data`
/// member of the JSON reply.
fn rpc_chip_temp(port: u16) -> Option<String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, RPC_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(RPC_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(RPC_TIMEOUT)).ok()?;
    stream.write_all(RPC_REQUEST.as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;

    let mut reply = Vec::new();
    let mut chunk = [0u8; 512];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                if let Some(end) = chunk[..read].iter().position(|byte| *byte == b'\n') {
                    reply.extend_from_slice(&chunk[..end]);
                    break;
                }
                reply.extend_from_slice(&chunk[..read]);
                if reply.len() > 64 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let line = String::from_utf8(reply).ok()?;
    extract_string_field(&line, "data")
}

/// Pull the JSON string member `field` out of a single-line reply, resolving
/// the escapes the manager emits (`\r`, `\n`, `\t`, `\"`, `\\`).
fn extract_string_field(line: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\":");
    let mut search = line;
    while let Some(at) = search.find(&needle) {
        let tail = &search[at + needle.len()..];
        let mut chars = tail.trim_start().chars();
        if chars.next()? != '"' {
            search = tail;
            continue;
        }
        let mut out = String::new();
        while let Some(char) = chars.next() {
            match char {
                '"' => return Some(out),
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('r') => out.push('\r'),
                    Some('t') => out.push('\t'),
                    Some('u') => {
                        for _ in 0..4 {
                            chars.next();
                        }
                        out.push('\u{fffd}');
                    }
                    Some(other) => out.push(other),
                    None => return None,
                },
                other => out.push(other),
            }
        }
        return None;
    }
    None
}

/// Reduce the twelve die readings to their hottest plausible value. Failed
/// sensors report the 65535 sentinel and are dropped like the manager's own
/// frontend does; a reply without a single usable reading is no reading.
fn chip_temp_max(data: &str) -> Option<f64> {
    let line = data
        .lines()
        .find(|line| line.trim_start().starts_with("^CHIPTEMP:"))?;
    let values = line.trim_start()["^CHIPTEMP:".len()..].trim();
    let mut hottest: Option<i32> = None;
    for field in values.split(',') {
        let Ok(raw) = field.trim().parse::<i32>() else {
            return None;
        };
        if raw == CHIPTEMP_FAIL_RAW || !(CHIPTEMP_RAW_MIN..=CHIPTEMP_RAW_MAX).contains(&raw) {
            continue;
        }
        hottest = hottest.max(Some(raw));
    }
    hottest.map(|raw| raw as f64 / 10.0)
}

/// Publish atomically the way the state snapshot does: temporary file first,
/// then a rename the readers can only observe whole.
fn store_cache(path: &Path, updated: u64, temperature: f64) -> Option<()> {
    let parent = path.parent()?;
    std::fs::create_dir_all(parent).ok()?;
    let temporary = path.with_extension(format!("{}.tmp", platform::process_id()));
    std::fs::write(
        &temporary,
        format!("temperature={temperature:.1}\nupdated={updated}\n"),
    )
    .ok()?;
    std::fs::rename(&temporary, path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;

    fn reply_with(data: &str) -> String {
        format!("{{\"id\":1,\"result\":{{\"data\":\"{data}\",\"error\":null,\"success\":true}}}}\n")
    }

    const LIVE_REPLY: &str = concat!(
        "{\"id\":1,\"result\":{\"data\":\"^CHIPTEMP: 421,407,397,427,420,420,",
        "440,450,450,460,430,430\\r\\nOK\",\"error\":null,\"success\":true}}\n"
    );

    #[test]
    fn extracts_the_data_member_with_escapes() {
        let line = reply_with("^CHIPTEMP: 421\\r\\nOK");
        assert_eq!(
            extract_string_field(&line, "data").as_deref(),
            Some("^CHIPTEMP: 421\r\nOK")
        );
        assert_eq!(extract_string_field(&line, "missing"), None);
        assert_eq!(extract_string_field("not json", "data"), None);
    }

    #[test]
    fn reduces_the_live_reply_to_the_hottest_die() {
        assert_eq!(chip_temp_max("garbage"), None);
        let data = extract_string_field(LIVE_REPLY, "data").unwrap();
        assert_eq!(chip_temp_max(&data), Some(46.0));
    }

    #[test]
    fn drops_failed_and_out_of_range_sensors() {
        let data = "^CHIPTEMP: 65535,65535,420,99999,-9999,430,430";
        assert_eq!(chip_temp_max(data), Some(43.0));
        assert_eq!(chip_temp_max("^CHIPTEMP: 65535,65535"), None);
        assert_eq!(chip_temp_max("^CHIPTEMP: 421,abc"), None);
    }

    #[test]
    fn stores_the_cache_atomically() {
        let root = std::env::temp_dir().join(format!(
            "h5fan-modem-store-{}-{}",
            std::process::id(),
            platform::wall_now()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths::sandbox(&root);
        assert_eq!(paths.mt5700_rpc_port, 0);

        assert_eq!(
            store_cache(&paths.module_cache, 1_700_000_000, 46.0),
            Some(())
        );
        let text = std::fs::read_to_string(&paths.module_cache).unwrap();
        assert_eq!(text, "temperature=46.0\nupdated=1700000000\n");
        let leftovers: Vec<_> = std::fs::read_dir(paths.module_cache.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn refresh_is_throttled_and_disabled_without_a_port() {
        let root = std::env::temp_dir().join(format!(
            "h5fan-modem-refresh-{}-{}",
            std::process::id(),
            platform::wall_now()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut paths = Paths::sandbox(&root);
        paths.mt5700_rpc_port = 5_701;

        let calls = std::cell::Cell::new(0u32);
        let query = |_: &Paths, _: u64| {
            calls.set(calls.get() + 1);
            None
        };
        let mut refresher = Refresher::default();

        refresher.refresh_with(&paths, 100, query);
        refresher.refresh_with(&paths, 115, query);
        assert_eq!(
            calls.get(),
            1,
            "within the refresh window nothing queries again"
        );

        refresher.refresh_with(&paths, 100 + REFRESH_SECS, query);
        assert_eq!(calls.get(), 2);

        let disabled = Paths::sandbox(&root);
        refresher.refresh_with(&disabled, 1_000, query);
        assert_eq!(calls.get(), 2, "port 0 disables the bridge entirely");
    }

    #[test]
    fn end_to_end_against_a_mock_manager() {
        use std::io::Write as _;

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut byte = [0u8; 1];
            while !request.ends_with(b"\n") {
                std::io::Read::read_exact(&mut socket, &mut byte).unwrap();
                request.push(byte[0]);
            }
            assert!(String::from_utf8_lossy(&request).contains("AT^CHIPTEMP?"));
            socket.write_all(LIVE_REPLY.as_bytes()).unwrap();
        });

        let root = std::env::temp_dir().join(format!(
            "h5fan-modem-e2e-{}-{}",
            std::process::id(),
            platform::wall_now()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let mut paths = Paths::sandbox(&root);
        paths.mt5700_rpc_port = port;

        assert_eq!(query_and_store(&paths, 1_700_000_100), Some(46.0));
        let text = std::fs::read_to_string(&paths.module_cache).unwrap();
        assert_eq!(text, "temperature=46.0\nupdated=1700000100\n");
        server.join().unwrap();
    }
}
