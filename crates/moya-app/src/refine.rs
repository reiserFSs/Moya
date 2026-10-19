//! Background HQ refinement: one worker thread, one throttled GPU lane, at most one job in flight.
//! The UI never waits for it: stale or unwanted jobs are cancelled with a flag and the
//! worker abandons them at its next dispatch; results are validated on the UI thread.
use eframe::egui;
use moya_gpu::{Chunking, Gpu};
use moya_pipeline::{RefineJob, RefineTicket, RefinedGlow};
use std::{
    sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc},
    time::Duration,
};

/// Row tiles and dispatch batches are capped at 16 Mpx and ~3 ms of estimated GPU time, and the
/// lane keeps the GPU at most half busy, so a UI frame (8 ms at 120 Hz) waits for at most one chunk.
const MAX_PIXELS: u64 = 16_000_000;
const CHUNK_TARGET: Duration = Duration::from_millis(3);
const DUTY: f32 = 0.5;

type Finished = Option<RefinedGlow>;

pub(super) struct Refiner {
    jobs: mpsc::Sender<(RefineJob, Arc<AtomicBool>)>,
    done: mpsc::Receiver<Finished>,
    flight: Option<(RefineTicket, Arc<AtomicBool>)>,
    stopped: bool,
}

impl Refiner {
    pub(super) fn new(gpu: Arc<Gpu>, ctx: egui::Context) -> Self {
        let (jobs, job_rx) = mpsc::channel::<(RefineJob, Arc<AtomicBool>)>();
        let (done_tx, done) = mpsc::channel();
        std::thread::Builder::new().name("moya-refine".into()).spawn(move || {
            // The lane's pool is bound to this thread, so it must be created and used here.
            let lane = Arc::new(gpu.lane());
            while let Ok((job, cancel)) = job_rx.recv() {
                lane.set_chunking(Some(Chunking {
                    max_pixels: MAX_PIXELS, target: CHUNK_TARGET, duty: DUTY, cancel,
                }));
                if done_tx.send(job.run(&lane)).is_err() { break; }
                ctx.request_repaint();
            }
        }).expect("spawn refine worker");
        Self { jobs, done, flight: None, stopped: false }
    }

    /// A job was started and its result (or cancellation) has not been collected yet.
    pub(super) fn busy(&self) -> bool { self.flight.is_some() }

    pub(super) fn stopped(&self) -> bool { self.stopped }

    pub(super) fn ticket(&self) -> Option<&RefineTicket> { self.flight.as_ref().map(|(ticket, _)| ticket) }

    pub(super) fn start(&mut self, job: RefineJob) {
        debug_assert!(self.flight.is_none());
        let cancel = Arc::new(AtomicBool::new(false));
        self.flight = Some((job.ticket().clone(), cancel.clone()));
        if self.jobs.send((job, cancel)).is_err() { self.flight = None; self.stopped = true; }
    }

    /// Ask the worker to abandon the job in flight; its acknowledgement arrives through `poll`.
    pub(super) fn cancel(&self) {
        if let Some((_, cancel)) = &self.flight { cancel.store(true, Ordering::Relaxed); }
    }

    /// Collect a finished job. A cancelled job yields nothing; a stale result is rejected by
    /// `Document::accept_refine`.
    pub(super) fn poll(&mut self) -> Option<RefinedGlow> {
        match self.done.try_recv() {
            Ok(result) => { self.flight = None; result }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => { self.flight = None; self.stopped = true; None }
        }
    }
}

impl Drop for Refiner {
    fn drop(&mut self) {
        // Detached on purpose: the worker sees the flag and the closed channel and exits by itself.
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use moya_core::{LayerSettings, Params};
    use moya_gpu::Depth;
    use moya_pipeline::{Document, LayerInputs, ViewFlags};
    use std::time::Instant;

    const VIEW: ViewFlags = ViewFlags { xray: false, solo: false, host_is_float: false };

    fn setup() -> (Arc<Gpu>, Document, Refiner, [LayerSettings; 1]) {
        let gpu = Arc::new(Gpu::new_blocking().expect("headless GPU"));
        let pixels: Vec<f32> = (0..96 * 64 * 4).map(|i| ((i * 37) % 101) as f32 / 100.).collect();
        let document = Document::new(gpu.clone(), gpu.upload_rgba32f(96, 64, &pixels), Depth::Eight);
        let refiner = Refiner::new(gpu.clone(), egui::Context::default());
        let layer = LayerSettings { id: 1, params: Params { radius: 2., glow_iterations: 1., ..Params::default() }, ..LayerSettings::default() };
        (gpu, document, refiner, [layer])
    }

    fn inputs(settings: &[LayerSettings; 1]) -> Vec<LayerInputs<'_>> {
        settings.iter().map(|settings| LayerInputs { settings, mask: None, mask_generation: 0 }).collect()
    }

    /// Poll until the job in flight is collected.
    fn collect(refiner: &mut Refiner) -> Option<RefinedGlow> {
        let start = Instant::now();
        while refiner.busy() {
            if let Some(glow) = refiner.poll() { return Some(glow); }
            assert!(start.elapsed() < Duration::from_secs(60), "refine worker timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    #[test]
    fn job_result_is_accepted() {
        let (_gpu, mut document, mut refiner, settings) = setup();
        let job = document.refine_job(&inputs(&settings), &VIEW).unwrap();
        refiner.start(job);
        assert!(refiner.busy() && refiner.ticket().is_some());
        let glow = collect(&mut refiner).expect("result");
        assert!(!refiner.busy());
        assert!(document.accept_refine(glow, &inputs(&settings), &VIEW));
    }

    #[test]
    fn cancelled_job_yields_nothing_and_worker_stays_usable() {
        let (_gpu, mut document, mut refiner, settings) = setup();
        refiner.start(document.refine_job(&inputs(&settings), &VIEW).unwrap());
        refiner.cancel();
        // The worker may have finished before seeing the flag; either way it must acknowledge.
        let _ = collect(&mut refiner);
        assert!(!refiner.busy() && !refiner.stopped());
        refiner.start(document.refine_job(&inputs(&settings), &VIEW).unwrap());
        let glow = collect(&mut refiner).expect("fresh job completes");
        assert!(document.accept_refine(glow, &inputs(&settings), &VIEW));
    }

    #[test]
    fn stale_result_is_discarded() {
        let (_gpu, mut document, mut refiner, mut settings) = setup();
        refiner.start(document.refine_job(&inputs(&settings), &VIEW).unwrap());
        settings[0].params.threshold += 0.2;
        assert!(!document.refine_current(refiner.ticket().unwrap(), &inputs(&settings), &VIEW));
        let glow = collect(&mut refiner).expect("worker still finishes");
        assert!(!document.accept_refine(glow, &inputs(&settings), &VIEW));
    }
}
