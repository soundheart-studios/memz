use crate::{Error, Result, state::StateRoot};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{TrySendError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
#[derive(Clone, Serialize, Deserialize)]
pub struct Binding {
    pub token: String,
    pub principal: String,
    pub session: String,
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub workers: usize,
    pub queue: usize,
    pub max_request_bytes: usize,
    pub timeout_ms: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            workers: 4,
            queue: 16,
            max_request_bytes: 131072,
            timeout_ms: 2000,
        }
    }
}
struct Owner {
    file: File,
    path: PathBuf,
    inode: u64,
}
impl Drop for Owner {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.inode) {
            let _ = fs::remove_file(&self.path);
        }
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}
pub struct Server {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}
impl Server {
    pub fn socket(&self) -> &Path {
        &self.path
    }
    pub fn start<F>(
        state: &StateRoot,
        bindings: Vec<Binding>,
        limits: Limits,
        handler: F,
    ) -> Result<Self>
    where
        F: Fn(&Binding, Value) -> Result<Value> + Send + Sync + 'static,
    {
        if limits.workers == 0
            || limits.workers > 32
            || limits.queue > 128
            || limits.max_request_bytes < 128
            || limits.max_request_bytes > 1048576
            || limits.timeout_ms == 0
            || limits.timeout_ms > 30000
        {
            return Err(Error::new(
                "invalid_limits",
                "invalid daemon resource bounds",
            ));
        }
        let state = StateRoot::open(&state.path)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(state.path.join("run/daemon.lock"))?;
        let meta = file.metadata()?;
        if !meta.is_file()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
            || meta.nlink() != 1
        {
            return Err(Error::new("unsafe_state", "invalid daemon lock"));
        }
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(Error::new("already_running", "daemon already owns state"));
        }
        let path = state.path.join("run/memz.sock");
        match fs::symlink_metadata(&path) {
            Ok(m) => {
                if !m.file_type().is_socket() || m.uid() != unsafe { libc::geteuid() } {
                    return Err(Error::new(
                        "unsafe_state",
                        "refusing unexpected socket path",
                    ));
                }
                fs::remove_file(&path)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let owner = Owner {
            file,
            path: path.clone(),
            inode: fs::metadata(&path)?.ino(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let handler = Arc::new(handler);
        let bindings = Arc::new(bindings);
        let join = thread::spawn(move || {
            let _owner = owner;
            let (sender, receiver) = sync_channel::<UnixStream>(limits.queue);
            let receiver = Arc::new(Mutex::new(receiver));
            let mut workers = Vec::new();
            for _ in 0..limits.workers {
                let rx = receiver.clone();
                let handler = handler.clone();
                let bindings = bindings.clone();
                workers.push(thread::spawn(move || {
                    loop {
                        let socket = match rx.lock().unwrap().recv() {
                            Ok(s) => s,
                            Err(_) => break,
                        };
                        let _ = serve(socket, &bindings, limits, &*handler);
                    }
                }));
            }
            while !stopping.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => match sender.try_send(stream) {
                        Ok(()) => {}
                        Err(TrySendError::Full(mut stream)) => {
                            let _ = stream.set_write_timeout(Some(Duration::from_millis(50)));
                            let _ = write_json(
                                &mut stream,
                                &json!({"error":{"code":"busy","message":"request queue is full"}}),
                            );
                        }
                        Err(TrySendError::Disconnected(_)) => break,
                    },
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
            drop(sender);
            for worker in workers {
                let _ = worker.join();
            }
        });
        Ok(Self {
            path,
            stop,
            join: Some(join),
        })
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
fn write_json(stream: &mut UnixStream, value: &Value) -> Result<()> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|_| Error::new("protocol", "invalid response"))?;
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    Ok(())
}
fn read_json(stream: &mut UnixStream, max: usize) -> Result<Value> {
    use std::io::Read;
    let mut bytes = Vec::new();
    BufReader::new(stream.take((max + 1) as u64)).read_until(b'\n', &mut bytes)?;
    if bytes.len() > max || !bytes.ends_with(b"\n") {
        return Err(Error::new(
            "invalid_request",
            "request exceeds framing bounds",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|_| Error::new("invalid_request", "invalid JSON frame"))
}
fn serve<F>(mut stream: UnixStream, bindings: &[Binding], limits: Limits, handler: &F) -> Result<()>
where
    F: Fn(&Binding, Value) -> Result<Value>,
{
    // Accepted sockets may inherit the nonblocking listener mode (notably on macOS).
    // The bounded worker uses blocking reads with an explicit deadline, not readiness polling.
    stream.set_nonblocking(false)?;
    let timeout = Some(Duration::from_millis(limits.timeout_ms));
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    let response = (|| {
        #[cfg(any(target_os = "macos", target_os = "freebsd"))]
        {
            let mut uid = 0;
            let mut gid = 0;
            if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0
                || uid != unsafe { libc::geteuid() }
            {
                return Err(Error::new("denied", "access is not authorized"));
            }
        }
        let value = read_json(&mut stream, limits.max_request_bytes)?;
        if value["version"].as_u64() != Some(1) {
            return Err(Error::new(
                "protocol_mismatch",
                "supported protocol version is 1",
            ));
        }
        let binding = bindings
            .iter()
            .find(|b| Some(b.token.as_str()) == value["token"].as_str())
            .ok_or_else(|| Error::new("denied", "access is not authorized"))?;
        handler(binding, value["body"].clone())
    })();
    let envelope = match response {
        Ok(value) => json!({"result":value}),
        Err(error) => json!({"error":{"code":error.code,"message":error.message}}),
    };
    write_json(&mut stream, &envelope)
}
pub fn request(path: &Path, token: &str, version: u64, body: Value) -> Result<Value> {
    let mut stream = UnixStream::connect(path)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    write_json(
        &mut stream,
        &json!({"version":version,"token":token,"body":body}),
    )?;
    let response = read_json(&mut stream, 1048576)?;
    if let Some(code) = response["error"]["code"].as_str() {
        let stable = match code {
            "denied" => "denied",
            "busy" => "busy",
            "protocol_mismatch" => "protocol_mismatch",
            "conflict" => "conflict",
            "deleted" => "deleted",
            "quota" => "quota",
            "too_large" => "too_large",
            "unavailable" => "unavailable",
            "revision_unavailable" => "revision_unavailable",
            "budget_too_small" => "budget_too_small",
            "export_limit" => "export_limit",
            "storage" => "storage",
            "cancelled" => "cancelled",
            "timeout" => "timeout",
            "semantic_deadline" => "semantic_deadline",
            "semantic_unavailable" => "semantic_unavailable",
            "provider_unavailable" => "provider_unavailable",
            "vector_unavailable" => "vector_unavailable",
            "quota_exceeded" => "quota_exceeded",
            "invalid_query" => "invalid_query",
            "invalid_budget" => "invalid_budget",
            "invalid_evidence" => "invalid_evidence",
            "invalid_filename" => "invalid_filename",
            "invalid_graph" => "invalid_graph",
            "invalid_input" => "invalid_input",
            "invalid_limit" => "invalid_limit",
            "invalid_relation" => "invalid_relation",
            "invalid_manifest" => "invalid_manifest",
            "stale_snapshot" => "stale_snapshot",
            "extraction_limit" => "extraction_limit",
            "generation_limit" => "generation_limit",
            "unsupported_schema" => "unsupported_schema",
            "damaged_schema" => "damaged_schema",
            "serialization" => "serialization",
            _ => "invalid_request",
        };
        return Err(Error::new(
            stable,
            response["error"]["message"]
                .as_str()
                .unwrap_or("request failed"),
        ));
    }
    Ok(response["result"].clone())
}
