//! Live event streaming over a non-blocking Unix domain socket.
//!
//! When running under systemd with `RuntimeDirectory=keyboard-debouncer`, the
//! daemon binds `/run/keyboard-debouncer/live.sock`. An interactive terminal can
//! attach with `sudo keyboard-debouncer --watch [--log-forward]` to view colored
//! debounce decisions with sub-millisecond IPC latency, without stopping the
//! service or polluting `journald` with ANSI escape sequences.

use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

pub const DEFAULT_SOCKET_PATH: &str = "/run/keyboard-debouncer/live.sock";

const FLAG_LOG_FORWARD: u8 = 0x01;
const FLAG_COLOR: u8 = 0x02;
const MAX_CLIENTS: usize = 16;

/// Pre-formatted plain and ANSI-colored representations of a single log event.
#[derive(Clone, Debug)]
pub struct LogLine {
    pub plain: String,
    pub colored: String,
}

impl LogLine {
    pub fn forward(ts: &str, is_down: bool, reason: &str) -> Self {
        let label = if is_down {
            "↓ FORWARD"
        } else {
            "↑ FORWARD"
        };
        Self {
            plain: format!("{ts} {label} {reason}"),
            colored: format!("\x1b[2m{ts}\x1b[0m \x1b[32m{label}\x1b[0m {reason}"),
        }
    }

    pub fn suppress(ts: &str, is_down: bool, reason: &str) -> Self {
        let label = if is_down {
            "↓ SUPPRESS"
        } else {
            "↑ SUPPRESS"
        };
        Self {
            plain: format!("{ts} {label} {reason}"),
            colored: format!("\x1b[2m{ts}\x1b[0m \x1b[1;31m{label}\x1b[0m {reason}"),
        }
    }

    pub fn as_str(&self, color: bool) -> &str {
        if color {
            &self.colored
        } else {
            &self.plain
        }
    }
}

struct WatcherClient {
    stream: UnixStream,
    log_forward: bool,
    color: bool,
    initialized: bool,
}

/// Non-blocking broadcaster for attached `--watch` clients.
pub struct LiveStream {
    listener: Option<UnixListener>,
    clients: Vec<WatcherClient>,
    stderr_color: bool,
}

impl LiveStream {
    /// Bind the live socket if `RUNTIME_DIRECTORY` is set by systemd.
    /// Falls back to a no-op stream in foreground runs or if binding fails.
    pub fn bind() -> Self {
        let Some(runtime_dir) = std::env::var_os("RUNTIME_DIRECTORY") else {
            return Self::disabled();
        };
        let Some(dir) = std::env::split_paths(&runtime_dir).next() else {
            return Self::disabled();
        };
        if dir.as_os_str().is_empty() {
            return Self::disabled();
        }
        let sock_path = dir.join("live.sock");
        match Self::bind_at(&sock_path) {
            Ok(stream) => stream,
            Err(e) => {
                eprintln!(
                    "⚠ Could not bind live watch socket at {}: {e}",
                    sock_path.display()
                );
                Self::disabled()
            }
        }
    }

    pub fn bind_at(sock_path: &Path) -> io::Result<Self> {
        let _ = std::fs::remove_file(sock_path);
        let listener = UnixListener::bind(sock_path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener: Some(listener),
            clients: Vec::new(),
            stderr_color: colored::control::SHOULD_COLORIZE.should_colorize(),
        })
    }

    pub fn disabled() -> Self {
        Self {
            listener: None,
            clients: Vec::new(),
            stderr_color: colored::control::SHOULD_COLORIZE.should_colorize(),
        }
    }

    /// Write a log line to the daemon's own `stderr` (colored only if `stderr` is a TTY).
    pub fn emit_stderr(&self, line: &LogLine) {
        eprintln!("{}", line.as_str(self.stderr_color));
    }

    /// Broadcast a log line to connected `--watch` clients.
    ///
    /// - `only_log_forward: None` sends to all initialized clients.
    /// - `only_log_forward: Some(true)` sends only to `--watch --log-forward` clients.
    /// - `only_log_forward: Some(false)` sends only to default `--watch` clients
    ///   (used when flushing buffered `pending` lines on a suppression so
    ///   `--log-forward` clients do not see duplicate `FORWARD` lines).
    pub fn broadcast(&mut self, line: &LogLine, only_log_forward: Option<bool>) {
        self.poll_clients();
        if self.clients.is_empty() {
            return;
        }

        let mut plain_line: Option<String> = None;
        let mut colored_line: Option<String> = None;

        self.clients.retain_mut(|client| {
            if !client.initialized {
                return true;
            }
            if let Some(required) = only_log_forward {
                if client.log_forward != required {
                    return true;
                }
            }

            let payload = if client.color {
                colored_line
                    .get_or_insert_with(|| format!("{}\n", line.colored))
                    .as_bytes()
            } else {
                plain_line
                    .get_or_insert_with(|| format!("{}\n", line.plain))
                    .as_bytes()
            };

            // Strictly non-blocking single write: if a client's socket buffer is full
            // (WouldBlock) or disconnected (BrokenPipe), drop the client immediately
            // so the keyboard filter loop never stalls.
            matches!(client.stream.write(payload), Ok(n) if n == payload.len())
        });
    }

    fn poll_clients(&mut self) {
        if let Some(listener) = &self.listener {
            while let Ok((stream, _)) = listener.accept() {
                if self.clients.len() >= MAX_CLIENTS {
                    drop(stream);
                    continue;
                }
                if stream.set_nonblocking(true).is_ok() {
                    self.clients.push(WatcherClient {
                        stream,
                        log_forward: false,
                        color: true,
                        initialized: false,
                    });
                }
            }
        }

        self.clients.retain_mut(|client| {
            let mut buf = [0u8; 1];
            match client.stream.read(&mut buf) {
                Ok(0) => false, // Client disconnected (EOF)
                Ok(1) => {
                    if !client.initialized {
                        client.log_forward = (buf[0] & FLAG_LOG_FORWARD) != 0;
                        client.color = (buf[0] & FLAG_COLOR) != 0;
                        client.initialized = true;
                    }
                    true
                }
                Ok(_) => true,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => true,
                Err(_) => false,
            }
        });
    }
}

fn default_socket_path() -> PathBuf {
    if let Some(runtime_dir) = std::env::var_os("RUNTIME_DIRECTORY") {
        if let Some(dir) = std::env::split_paths(&runtime_dir).next() {
            if !dir.as_os_str().is_empty() {
                return dir.join("live.sock");
            }
        }
    }
    PathBuf::from(DEFAULT_SOCKET_PATH)
}

/// Connect to the running daemon's live socket and stream events to `stdout`.
pub fn run_client(log_forward: bool) -> Result<(), Box<dyn std::error::Error>> {
    let sock_path = default_socket_path();
    let mut stream = UnixStream::connect(&sock_path).map_err(|e| match e.kind() {
        io::ErrorKind::PermissionDenied => format!(
            "Permission denied connecting to {}.\n\
             Run with sudo: sudo keyboard-debouncer --watch{}",
            sock_path.display(),
            if log_forward { " --log-forward" } else { "" }
        ),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => format!(
            "Cannot connect to {} — is keyboard-debouncer.service running?",
            sock_path.display()
        ),
        _ => format!("Failed to connect to {}: {e}", sock_path.display()),
    })?;

    let mut flags = 0u8;
    if log_forward {
        flags |= FLAG_LOG_FORWARD;
    }
    if colored::control::SHOULD_COLORIZE.should_colorize() {
        flags |= FLAG_COLOR;
    }
    stream.write_all(&[flags])?;

    if log_forward {
        eprintln!(
            "Watching keyboard-debouncer live stream (all target events)… (Ctrl-C to detach)\n"
        );
    } else {
        eprintln!("Watching keyboard-debouncer live stream… (Ctrl-C to detach)\n");
    }

    let mut stdout = io::stdout();
    let mut buf = [0u8; 4096];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            eprintln!("Daemon closed live stream.");
            return Ok(());
        }
        stdout.write_all(&buf[..n])?;
        stdout.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_sock_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "kbd-debouncer-test-{label}-{}-{nanos}.sock",
            std::process::id()
        ))
    }

    #[test]
    fn broadcast_filters_by_client_mode_and_color_preference() {
        let sock_path = temp_sock_path("modes");
        let mut server = LiveStream::bind_at(&sock_path).unwrap();

        // Client 1: default mode (suppressions + context), plain text
        let mut c_default = UnixStream::connect(&sock_path).unwrap();
        c_default.set_nonblocking(true).unwrap();
        c_default.write_all(&[0]).unwrap();

        // Client 2: log_forward mode, ANSI colored
        let mut c_forward = UnixStream::connect(&sock_path).unwrap();
        c_forward.set_nonblocking(true).unwrap();
        c_forward
            .write_all(&[FLAG_LOG_FORWARD | FLAG_COLOR])
            .unwrap();

        let fwd = LogLine::forward("12:00:00.000", true, "KEY_K  gap=45.00ms");
        let sup = LogLine::suppress("12:00:00.010", true, "KEY_K  gap=10.00ms [chatter]");

        // 1. Live forward when daemon log_forward=false -> only c_forward receives it
        server.broadcast(&fwd, Some(true));

        let mut buf = [0u8; 256];
        assert_eq!(
            c_default.read(&mut buf).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let n = c_forward.read(&mut buf).unwrap();
        let got_fwd = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(got_fwd.contains("\x1b[32m↓ FORWARD\x1b[0m"));

        // 2. Pending flush on suppress -> only c_default receives the buffered forward
        server.broadcast(&fwd, Some(false));
        // Followed by suppress -> both clients receive it
        server.broadcast(&sup, None);

        let n_def = c_default.read(&mut buf).unwrap();
        let got_def = std::str::from_utf8(&buf[..n_def]).unwrap();
        assert_eq!(got_def, format!("{}\n{}\n", fwd.plain, sup.plain));

        let n_fwd = c_forward.read(&mut buf).unwrap();
        let got_sup = std::str::from_utf8(&buf[..n_fwd]).unwrap();
        assert_eq!(got_sup, format!("{}\n", sup.colored));

        let _ = std::fs::remove_file(&sock_path);
    }

    #[test]
    fn disconnected_clients_are_pruned_without_error() {
        let sock_path = temp_sock_path("prune");
        let mut server = LiveStream::bind_at(&sock_path).unwrap();

        let mut client = UnixStream::connect(&sock_path).unwrap();
        client.write_all(&[FLAG_LOG_FORWARD]).unwrap();
        drop(client);

        let line = LogLine::suppress("12:00:00.000", true, "KEY_K");
        server.broadcast(&line, None);
        assert_eq!(server.clients.len(), 0);

        let _ = std::fs::remove_file(&sock_path);
    }
}
