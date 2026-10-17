//! Background decode and cancellable, exclusively-owned bridge output.
use eframe::egui;
use moya_gpu::{Gpu, Image};
use moya_io::{Picture, SampleDepth};
use parking_lot::{Condvar, Mutex};
use std::{
    cell::Cell,
    path::{Path, PathBuf},
    sync::{atomic::{AtomicU64, Ordering}, mpsc, Arc},
};

type Loaded = (u64, PathBuf, anyhow::Result<Picture>);
#[derive(Default)]
struct DecodeState {
    generation: u64,
    pending: Option<(u64, PathBuf)>,
    stopped: bool,
}
struct DecodeQueue {
    state: Mutex<DecodeState>,
    ready: Condvar,
}
impl DecodeQueue {
    fn current(&self, generation: u64) -> bool {
        let state = self.state.lock();
        !state.stopped && state.generation == generation
    }
}

pub struct Loader {
    queue: Arc<DecodeQueue>,
    results: mpsc::Receiver<Loaded>,
}
impl Loader {
    pub fn new(ctx: egui::Context) -> Self {
        Self::with_decoder(ctx, |path| moya_io::load(path))
    }
    fn with_decoder<F>(ctx: egui::Context, decode: F) -> Self
    where F: FnMut(&Path) -> anyhow::Result<Picture> + Send + 'static {
        let queue = Arc::new(DecodeQueue {
            state: Mutex::new(DecodeState::default()), ready: Condvar::new(),
        });
        let (tx, results) = mpsc::channel();
        let worker = queue.clone();
        std::thread::spawn(move || decode_loop(&worker, decode, |result| {
            let _ = tx.send(result);
            ctx.request_repaint();
        }));
        Self { queue, results }
    }
    pub fn request(&self, path: PathBuf) -> u64 {
        let mut state = self.queue.state.lock();
        state.generation = state.generation.wrapping_add(1);
        let generation = state.generation;
        state.pending = Some((generation, path));
        self.queue.ready.notify_one();
        generation
    }
    pub fn invalidate(&self) {
        let mut state = self.queue.state.lock();
        state.generation = state.generation.wrapping_add(1);
        state.pending = None;
    }
    pub fn poll(&self) -> Option<Loaded> {
        while let Ok(result) = self.results.try_recv() {
            if self.queue.current(result.0) { return Some(result); }
        }
        None
    }
}
impl Drop for Loader {
    fn drop(&mut self) {
        let mut state = self.queue.state.lock();
        state.stopped = true;
        state.pending = None;
        self.queue.ready.notify_one();
        // Detach: an active codec finishes on its worker, never on the UI thread.
    }
}
fn decode_loop<F, S>(queue: &DecodeQueue, mut decode: F, mut send: S)
where F: FnMut(&Path) -> anyhow::Result<Picture>, S: FnMut(Loaded) {
    loop {
        let job = {
            let mut state = queue.state.lock();
            while state.pending.is_none() && !state.stopped {
                queue.ready.wait(&mut state);
            }
            if state.stopped { return; }
            state.pending.take().unwrap()
        };
        if let Some(result) = decode_job(queue, job, &mut decode) { send(result); }
    }
}
fn decode_job<F>(queue: &DecodeQueue, (generation, path): (u64, PathBuf), decode: &mut F) -> Option<Loaded>
where F: FnMut(&Path) -> anyhow::Result<Picture> {
    if !queue.current(generation) { return None; }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode(&path)))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("image decoder panicked")));
    queue.current(generation).then_some((generation, path, result))
}

/// GPU-backed export; only the encoder's current band is downloaded to the CPU.
pub struct ExportImage {
    pub image: Image,
    pub depth: SampleDepth,
    pub icc_profile: Option<Vec<u8>>,
    pub opaque: bool,
}
impl ExportImage {
    pub fn save(&self, gpu: &Gpu, path: &Path) -> anyhow::Result<()> {
        moya_io::save_streaming(
            path, self.image.width(), self.image.height(), self.depth,
            self.icc_profile.as_deref(),
            |y0, rows, rgba| {
                gpu.download_row_range(&self.image, y0, rows, rgba)?;
                if self.opaque {
                    for pixel in rgba.chunks_exact_mut(4) {
                        pixel[0] *= pixel[3];
                        pixel[1] *= pixel[3];
                        pixel[2] *= pixel[3];
                        pixel[3] = 1.0;
                    }
                }
                Ok(())
            },
        )
    }
}

#[derive(Default)]
struct OutputState {
    cancelled: bool,
    produced: Option<PathBuf>,
}
impl OutputState {
    fn cleanup(&mut self) {
        if let Some(path) = self.produced.take() { let _ = std::fs::remove_file(path); }
    }
}
impl Drop for OutputState {
    fn drop(&mut self) { self.cleanup(); }
}
struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); }
}

/// Keep this handle until Photoshop accepts the result. Dropping it cancels and
/// removes only this worker's output, including a result already in the channel.
pub struct SendWriter {
    state: Arc<Mutex<OutputState>>,
    results: mpsc::Receiver<anyhow::Result<()>>,
    completed: Cell<bool>,
}
impl SendWriter {
    pub fn new(ctx: egui::Context, gpu: Arc<Gpu>, path: PathBuf, image: ExportImage) -> Self {
        Self::with_writer(ctx, path, move |staging| image.save(&gpu, staging))
    }
    fn with_writer<F>(ctx: egui::Context, path: PathBuf, write: F) -> Self
    where F: FnOnce(&Path) -> anyhow::Result<()> + Send + 'static {
        let state = Arc::new(Mutex::new(OutputState::default()));
        let worker = state.clone();
        let (tx, results) = mpsc::channel();
        std::thread::spawn(move || {
            let result = write_output(&worker, &path, write);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
        Self { state, results, completed: Cell::new(false) }
    }
    pub fn poll(&self) -> Option<anyhow::Result<()>> {
        if self.completed.get() { return None; }
        match self.results.try_recv() {
            Ok(result) => {
                self.completed.set(true);
                Some(result)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.completed.set(true);
                Some(Err(anyhow::anyhow!("output writer stopped")))
            }
        }
    }
    pub fn cancel(&self) {
        let mut state = self.state.lock();
        state.cancelled = true;
        state.cleanup();
    }
    /// Transfer ownership to the host only after it has accepted the output.
    pub fn keep_output(&self) { self.state.lock().produced = None; }
}
impl Drop for SendWriter {
    fn drop(&mut self) { self.cancel(); }
}

/// Own every TIFF and the project until the host acknowledges the whole result.
pub struct SendBatch {
    writers: Vec<SendWriter>,
    remaining: Cell<usize>,
    completed: Cell<bool>,
}
impl SendBatch {
    pub fn new(
        ctx: egui::Context,
        gpu: Arc<Gpu>,
        images: Vec<(PathBuf, ExportImage)>,
        project_path: PathBuf,
        project: moya_project::Project,
    ) -> Self {
        let mut writers = Vec::with_capacity(images.len() + 1);
        for (path, image) in images {
            writers.push(SendWriter::new(ctx.clone(), gpu.clone(), path, image));
        }
        writers.push(SendWriter::with_writer(ctx, project_path, move |staging| {
            moya_project::save(staging, &project)
        }));
        Self { remaining: Cell::new(writers.len()), writers, completed: Cell::new(false) }
    }
    pub fn poll(&self) -> Option<anyhow::Result<()>> {
        if self.completed.get() { return None; }
        for writer in &self.writers {
            if let Some(result) = writer.poll() {
                if let Err(error) = result {
                    self.completed.set(true);
                    self.cancel();
                    return Some(Err(error));
                }
                self.remaining.set(self.remaining.get() - 1);
            }
        }
        if self.remaining.get() == 0 {
            self.completed.set(true);
            Some(Ok(()))
        } else { None }
    }
    pub fn cancel(&self) {
        for writer in &self.writers { writer.cancel(); }
    }
    pub fn keep_output(&self) {
        for writer in &self.writers { writer.keep_output(); }
    }
}
fn write_output<F>(state: &Mutex<OutputState>, path: &Path, write: F) -> anyhow::Result<()>
where F: FnOnce(&Path) -> anyhow::Result<()> {
    anyhow::ensure!(!state.lock().cancelled, "output cancelled");
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let staging = loop {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!(".moya-{}-{serial}.{}", std::process::id(), path.extension().unwrap_or_default().to_string_lossy());
        let candidate = path.with_file_name(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(_) => break Staging(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    };
    anyhow::ensure!(!state.lock().cancelled, "output cancelled");
    write(&staging.0)?;
    let mut state = state.lock();
    anyhow::ensure!(!state.cancelled, "output cancelled");
    // Exclusive publication: never overwrite or later delete a pre-existing file.
    std::fs::hard_link(&staging.0, path)?;
    state.produced = Some(path.to_owned());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn queue() -> DecodeQueue {
        DecodeQueue { state: Mutex::new(DecodeState { generation: 2, ..Default::default() }), ready: Condvar::new() }
    }
    #[test]
    fn stale_before_and_after_decode() {
        let queue = queue();
        assert!(decode_job(&queue, (1, "old".into()), &mut |_| panic!("stale job decoded")).is_none());
        let mut calls = 0;
        let result = decode_job(&queue, (2, "active".into()), &mut |_| {
            calls += 1;
            queue.state.lock().generation = 3;
            Err(anyhow::anyhow!("obsolete"))
        });
        assert!(result.is_none());
        assert_eq!(calls, 1);
    }
    #[test]
    fn loader_decodes_only_first_and_newest_pending_serially() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let active = Arc::new(AtomicU64::new(0));
        let maximum = Arc::new(AtomicU64::new(0));
        let worker_calls = calls.clone();
        let worker_active = active.clone();
        let worker_maximum = maximum.clone();
        let loader = Loader::with_decoder(egui::Context::default(), move |path| {
            let in_flight = worker_active.fetch_add(1, Ordering::SeqCst) + 1;
            worker_maximum.fetch_max(in_flight, Ordering::SeqCst);
            worker_calls.lock().push(path.to_owned());
            started_tx.send(path.to_owned()).unwrap();
            release_rx.recv().unwrap();
            worker_active.fetch_sub(1, Ordering::SeqCst);
            Err(anyhow::anyhow!("injected decode result"))
        });
        let timeout = std::time::Duration::from_secs(5);
        loader.request("first".into());
        assert_eq!(started_rx.recv_timeout(timeout).unwrap(), PathBuf::from("first"));
        loader.request("second".into());
        let newest = loader.request("third".into());
        release_tx.send(()).unwrap();
        assert_eq!(started_rx.recv_timeout(timeout).unwrap(), PathBuf::from("third"));
        release_tx.send(()).unwrap();
        let (generation, path, result) = loader.results.recv_timeout(timeout).unwrap();
        assert_eq!(generation, newest);
        assert_eq!(path, PathBuf::from("third"));
        assert!(result.is_err());
        assert!(loader.poll().is_none());
        assert_eq!(*calls.lock(), vec![PathBuf::from("first"), PathBuf::from("third")]);
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }
    fn test_path() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!("moya-writer-test-{}-{}.tif", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)))
    }
    #[test]
    fn streaming_export_matches_legacy_pixels_and_icc() {
        let gpu = Gpu::new_blocking().expect("GPU is mandatory for numerical tests");
        let pixels = [
            -0.25, 0.5, 2.0, 0.25, 0.75, 0.125, 0.5, 0.0,
            0.5, 0.25, 0.75, 1.0, 2.0, -1.0, 0.5, 0.5,
            0.125, 0.875, 0.25, 0.75, 0.0, 1.0, 0.5, 1.0,
        ];
        let image = gpu.upload_rgba32f(3, 2, &pixels);
        let icc = Some(b"test opaque ICC profile payload".to_vec());
        for depth in [SampleDepth::U8, SampleDepth::U16, SampleDepth::F32] {
            for opaque in [false, true] {
                let streamed = test_path();
                let legacy = test_path();
                let export = ExportImage { image: image.clone(), depth, opaque, icc_profile: icc.clone() };
                export.save(&gpu, &streamed).unwrap();
                let mut rgba = gpu.download_rgba32f(&image);
                if opaque {
                    for pixel in rgba.chunks_exact_mut(4) {
                        pixel[0] *= pixel[3];
                        pixel[1] *= pixel[3];
                        pixel[2] *= pixel[3];
                        pixel[3] = 1.0;
                    }
                }
                let picture = Picture { width: 3, height: 2, depth, rgba, icc_profile: icc.clone() };
                moya_io::save(&legacy, &picture, depth).unwrap();
                assert_eq!(std::fs::read(&streamed).unwrap(), std::fs::read(&legacy).unwrap(), "{depth:?}, opaque={opaque}");
                let actual = moya_io::load(&streamed).unwrap();
                let expected = moya_io::load(&legacy).unwrap();
                assert_eq!((actual.width, actual.height, actual.depth), (3, 2, depth));
                assert_eq!(actual.icc_profile, icc);
                assert_eq!(actual.rgba, expected.rgba, "{depth:?}, opaque={opaque}");
                std::fs::remove_file(streamed).unwrap();
                std::fs::remove_file(legacy).unwrap();
            }
        }
    }
    #[test]
    fn cancelled_before_and_during_write_cleans_only_owned_files() {
        let path = test_path();
        std::fs::write(&path, b"user file").unwrap();
        let state = Mutex::new(OutputState { cancelled: true, produced: None });
        assert!(write_output(&state, &path, |_| panic!("cancelled write ran")).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"user file");
        state.lock().cancelled = false;
        let mut staging_path = None;
        assert!(write_output(&state, &path, |staging| {
            staging_path = Some(staging.to_owned());
            std::fs::write(staging, b"partial")?;
            state.lock().cancelled = true;
            Ok(())
        }).is_err());
        assert!(!staging_path.unwrap().exists());
        assert_eq!(std::fs::read(&path).unwrap(), b"user file");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn cancellation_after_result_sent_removes_output() {
        let path = test_path();
        let state = Arc::new(Mutex::new(OutputState::default()));
        let (tx, results) = mpsc::channel();
        tx.send(write_output(&state, &path, |staging| {
            std::fs::write(staging, b"finished")?;
            Ok(())
        })).unwrap();
        let writer = SendWriter { state, results, completed: Cell::new(false) };
        assert!(path.exists());
        writer.cancel();
        assert!(!path.exists());
        assert!(writer.poll().unwrap().is_ok());
    }
    #[test]
    fn publication_failure_preserves_existing_file() {
        let path = test_path();
        std::fs::write(&path, b"user file").unwrap();
        let state = Mutex::new(OutputState::default());
        assert!(write_output(&state, &path, |staging| {
            std::fs::write(staging, b"new")?;
            Ok(())
        }).is_err());
        drop(state);
        assert_eq!(std::fs::read(&path).unwrap(), b"user file");
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn batch_cancellation_cleans_finished_outputs_and_active_writer() {
        let finished = test_path();
        let project = test_path().with_extension("moya");
        let active = test_path();
        let mut writers = Vec::new();
        for path in [&finished, &project] {
            let state = Arc::new(Mutex::new(OutputState::default()));
            let (tx, results) = mpsc::channel();
            tx.send(write_output(&state, path, |staging| {
                std::fs::write(staging, b"finished")?;
                Ok(())
            })).unwrap();
            writers.push(SendWriter { state, results, completed: Cell::new(false) });
        }
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let state = Arc::new(Mutex::new(OutputState::default()));
        let worker = state.clone();
        let output = active.clone();
        let (tx, results) = mpsc::channel();
        std::thread::spawn(move || {
            let result = write_output(&worker, &output, |staging| {
                std::fs::write(staging, b"in flight")?;
                started_tx.send(staging.to_owned()).unwrap();
                release_rx.recv().unwrap();
                Ok(())
            });
            tx.send(result).unwrap();
            done_tx.send(()).unwrap();
        });
        writers.push(SendWriter { state, results, completed: Cell::new(false) });
        let batch = SendBatch { remaining: Cell::new(writers.len()), writers, completed: Cell::new(false) };
        let timeout = std::time::Duration::from_secs(5);
        let staging = started_rx.recv_timeout(timeout).unwrap();
        assert!(batch.poll().is_none());
        batch.cancel();
        assert!(!finished.exists());
        assert!(!project.exists());
        release_tx.send(()).unwrap();
        done_rx.recv_timeout(timeout).unwrap();
        assert!(!active.exists());
        assert!(!staging.exists());
        assert!(batch.poll().unwrap().is_err());
    }
}
