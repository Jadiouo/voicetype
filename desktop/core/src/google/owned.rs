//! Keep the input owner with the CLI cleanup worker. A resident host may be
//! dropped after an error, but the hook must outlive an unobserved CLI close.
use super::{GoogleBoundary, GoogleCaptureOwnership};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

pub(crate) struct OwnedGoogleBoundary<B: GoogleBoundary + Send + 'static, L: Send + 'static> {
    boundary: Option<B>,
    lease: Option<L>,
    released: Arc<AtomicBool>,
    cleanup_confirmed: bool,
}

impl<B: GoogleBoundary + Send + 'static, L: Send + 'static> OwnedGoogleBoundary<B, L> {
    pub(crate) fn new(boundary: B, lease: L, released: Arc<AtomicBool>) -> Self {
        Self {
            boundary: Some(boundary),
            lease: Some(lease),
            released,
            cleanup_confirmed: false,
        }
    }
}

impl<B: GoogleBoundary + Send + 'static, L: Send + 'static> Drop for OwnedGoogleBoundary<B, L> {
    fn drop(&mut self) {
        let Some(boundary) = self.boundary.take() else {
            return;
        };
        let lease = self.lease.take().unwrap();
        if self.cleanup_confirmed {
            drop(boundary);
            drop(lease);
            self.released.store(true, Ordering::Release);
            return;
        }
        // Also cover a failed provider thread spawn or worker unwind. Never
        // publish release merely because Drop ran. The Arc survives a thread
        // creation failure by leaking the owner rather than dropping the hook.
        start_cleanup_retry(boundary, lease, self.released.clone(), |task| {
            thread::Builder::new()
                .name("voicetype-google-cleanup".into())
                .spawn(move || task())
                .map(|_| ())
        });
    }
}

fn start_cleanup_retry<B: GoogleBoundary + Send + 'static, L: Send + 'static>(
    boundary: B,
    lease: L,
    released: Arc<AtomicBool>,
    spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> io::Result<()>,
) {
    let owner = Arc::new(Mutex::new(Some((boundary, lease))));
    let retry = owner.clone();
    let task = Box::new(move || {
        let (mut boundary, lease) = retry.lock().unwrap().take().unwrap();
        loop {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| boundary.cleanup()));
            if matches!(result, Ok(Ok(()))) {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        drop(boundary);
        drop(lease);
        released.store(true, Ordering::Release);
    });
    if spawn(task).is_err() {
        // The spawning closure was dropped; `owner` is the sole remaining
        // owner. Preserve it permanently if the OS cannot create a retry
        // thread. No release is reported and no second microphone starts.
        std::mem::forget(owner);
    }
}

impl<B: GoogleBoundary + Send + 'static, L: Send + 'static> GoogleBoundary
    for OwnedGoogleBoundary<B, L>
{
    fn capture_ownership(&self) -> GoogleCaptureOwnership {
        self.boundary.as_ref().unwrap().capture_ownership()
    }
    fn set_cancellation(&mut self, flag: Arc<AtomicBool>) {
        self.boundary.as_mut().unwrap().set_cancellation(flag);
    }
    fn start_capture(&mut self) -> io::Result<()> {
        self.cleanup_confirmed = false;
        self.boundary.as_mut().unwrap().start_capture()
    }
    fn stop_capture_and_drain(&mut self) -> io::Result<()> {
        self.boundary.as_mut().unwrap().stop_capture_and_drain()
    }
    fn stop_official_voice(&mut self) -> io::Result<()> {
        self.boundary.as_mut().unwrap().stop_official_voice()
    }
    fn wait_official_recorder(&mut self) -> io::Result<()> {
        self.boundary.as_mut().unwrap().wait_official_recorder()
    }
    fn capture_editor(&mut self) -> io::Result<String> {
        self.boundary.as_mut().unwrap().capture_editor()
    }
    fn settle(&mut self, duration: Duration) -> io::Result<()> {
        self.boundary.as_mut().unwrap().settle(duration)
    }
    fn reject_known_voice_errors(&mut self) -> io::Result<()> {
        self.boundary.as_mut().unwrap().reject_known_voice_errors()
    }
    fn cleanup(&mut self) -> io::Result<()> {
        self.boundary.as_mut().unwrap().cleanup()?;
        self.cleanup_confirmed = true;
        Ok(())
    }
    fn take_review_pcm(&mut self) -> Option<Vec<u8>> {
        self.boundary.as_mut().unwrap().take_review_pcm()
    }
    fn prewarm_next(&mut self) -> io::Result<()> {
        self.cleanup_confirmed = false;
        self.boundary.as_mut().unwrap().prewarm_next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::google::GoogleProvider;
    use std::{
        sync::atomic::{AtomicU8, AtomicUsize},
        thread,
        time::Instant,
    };

    struct Closing {
        phase: Arc<AtomicU8>,
        attempts: Arc<AtomicUsize>,
    }
    impl GoogleBoundary for Closing {
        fn capture_ownership(&self) -> GoogleCaptureOwnership {
            GoogleCaptureOwnership::OfficialCliNative
        }
        fn start_capture(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn stop_capture_and_drain(&mut self) -> io::Result<()> {
            unreachable!()
        }
        fn stop_official_voice(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn wait_official_recorder(&mut self) -> io::Result<()> {
            unreachable!()
        }
        fn capture_editor(&mut self) -> io::Result<String> {
            Ok(String::new())
        }
        fn settle(&mut self, _: Duration) -> io::Result<()> {
            Ok(())
        }
        fn reject_known_voice_errors(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn cleanup(&mut self) -> io::Result<()> {
            self.attempts.fetch_add(1, Ordering::AcqRel);
            match self.phase.load(Ordering::Acquire) {
                0 => Err(io::ErrorKind::TimedOut.into()), // delayed ConPTY close
                1 => Err(io::ErrorKind::PermissionDenied.into()), // Job termination failed
                _ => Ok(()),
            }
        }
    }
    struct Lease(Arc<AtomicBool>);
    impl Drop for Lease {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }

    fn wait_for_attempts(attempts: &AtomicUsize, at_least: usize) {
        let until = Instant::now() + Duration::from_secs(2);
        while attempts.load(Ordering::Acquire) < at_least && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(attempts.load(Ordering::Acquire) >= at_least);
    }

    #[test]
    fn detached_cleanup_keeps_input_owner_through_delayed_close_and_termination_error() {
        let phase = Arc::new(AtomicU8::new(0));
        let attempts = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicBool::new(false));
        let lease_dropped = Arc::new(AtomicBool::new(false));
        let boundary = OwnedGoogleBoundary::new(
            Closing {
                phase: phase.clone(),
                attempts: attempts.clone(),
            },
            Lease(lease_dropped.clone()),
            released.clone(),
        );
        assert_eq!(
            boundary.capture_ownership(),
            GoogleCaptureOwnership::OfficialCliNative
        );
        let provider = GoogleProvider::spawn(boundary).unwrap();
        drop(provider);
        wait_for_attempts(&attempts, 1);
        assert!(!released.load(Ordering::Acquire));
        assert!(!lease_dropped.load(Ordering::Acquire));
        phase.store(1, Ordering::Release);
        let count = attempts.load(Ordering::Acquire);
        wait_for_attempts(&attempts, count + 1);
        assert!(!released.load(Ordering::Acquire));
        assert!(!lease_dropped.load(Ordering::Acquire));
        phase.store(2, Ordering::Release);
        let until = Instant::now() + Duration::from_secs(2);
        while !released.load(Ordering::Acquire) && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(released.load(Ordering::Acquire));
        assert!(lease_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn unwind_before_confirmed_cleanup_detaches_retry_without_releasing_input() {
        let phase = Arc::new(AtomicU8::new(0));
        let attempts = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicBool::new(false));
        let lease_dropped = Arc::new(AtomicBool::new(false));
        let until = Instant::now() + Duration::from_secs(2);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _boundary = OwnedGoogleBoundary::new(
                Closing {
                    phase: phase.clone(),
                    attempts: attempts.clone(),
                },
                Lease(lease_dropped.clone()),
                released.clone(),
            );
            panic!("synthetic provider unwind");
        }));
        assert!(result.is_err());
        assert!(Instant::now() < until, "Drop blocked the resident thread");
        wait_for_attempts(&attempts, 1);
        assert!(!released.load(Ordering::Acquire));
        assert!(!lease_dropped.load(Ordering::Acquire));
        phase.store(2, Ordering::Release);
        while !released.load(Ordering::Acquire) && Instant::now() < until {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(released.load(Ordering::Acquire));
        assert!(lease_dropped.load(Ordering::Acquire));
    }

    #[test]
    fn failed_retry_thread_spawn_leaks_owner_and_never_publishes_release() {
        let released = Arc::new(AtomicBool::new(false));
        let lease_dropped = Arc::new(AtomicBool::new(false));
        start_cleanup_retry(
            Closing {
                phase: Arc::new(AtomicU8::new(0)),
                attempts: Arc::new(AtomicUsize::new(0)),
            },
            Lease(lease_dropped.clone()),
            released.clone(),
            |_task| Err(io::ErrorKind::OutOfMemory.into()),
        );
        assert!(!released.load(Ordering::Acquire));
        assert!(!lease_dropped.load(Ordering::Acquire));
    }
}
