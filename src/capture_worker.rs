use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::capture::{Capture, CaptureRequest};
use crate::config::RetrievalMethod;

struct CaptureData {
    text: String,
    segments: Vec<String>,
    rect: RECT,
    hwnd: usize,
    title: String,
    method: RetrievalMethod,
    menu: bool,
}

impl From<Capture> for CaptureData {
    fn from(capture: Capture) -> Self {
        Self {
            text: capture.text,
            segments: capture.segments,
            rect: capture.rect,
            hwnd: capture.hwnd.0 as usize,
            title: capture.title,
            method: capture.method,
            menu: capture.menu,
        }
    }
}

impl From<CaptureData> for Capture {
    fn from(capture: CaptureData) -> Self {
        Self {
            text: capture.text,
            segments: capture.segments,
            rect: capture.rect,
            hwnd: HWND(capture.hwnd as _),
            title: capture.title,
            method: capture.method,
            menu: capture.menu,
        }
    }
}

#[derive(Default)]
struct WorkerState {
    pending: Option<(u64, CaptureRequest)>,
    stopped: bool,
}

pub(crate) struct CaptureWorker {
    state: Arc<(Mutex<WorkerState>, Condvar)>,
    results: mpsc::Receiver<(u64, Option<CaptureData>)>,
}

impl CaptureWorker {
    pub fn start(hwnd: HWND, notify: u32) -> Option<Self> {
        let state = Arc::new((Mutex::new(WorkerState::default()), Condvar::new()));
        let worker_state = Arc::clone(&state);
        let (sender, results) = mpsc::channel();
        let hwnd = hwnd.0 as usize;
        thread::Builder::new()
            .name("textify-rs-capture".to_string())
            .spawn(move || {
                let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
                Self::run_worker(
                    worker_state,
                    sender,
                    |request| {
                        if initialized {
                            request.execute().map(CaptureData::from)
                        } else {
                            None
                        }
                    },
                    || unsafe {
                        let _ = PostMessageW(HWND(hwnd as _), notify, WPARAM(0), LPARAM(0));
                    },
                );
                if initialized {
                    unsafe { CoUninitialize() };
                }
            })
            .ok()?;
        Some(Self { state, results })
    }

    fn run_worker(
        worker_state: Arc<(Mutex<WorkerState>, Condvar)>,
        sender: mpsc::Sender<(u64, Option<CaptureData>)>,
        mut capture: impl FnMut(CaptureRequest) -> Option<CaptureData>,
        notify: impl Fn(),
    ) {
        loop {
            let request = {
                let (lock, wake) = &*worker_state;
                let mut state = lock.lock().unwrap();
                while state.pending.is_none() && !state.stopped {
                    state = wake.wait(state).unwrap();
                }
                if state.stopped {
                    break;
                }
                state.pending.take().unwrap()
            };
            let result = capture(request.1);
            let (lock, _) = &*worker_state;
            let state = lock.lock().unwrap();
            if state.stopped {
                break;
            }
            if sender.send((request.0, result)).is_err() {
                break;
            }
            notify();
        }
    }

    pub fn submit(&self, id: u64, request: CaptureRequest) -> bool {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap();
        if state.stopped {
            return false;
        }
        state.pending = Some((id, request));
        wake.notify_one();
        true
    }

    pub fn try_result(&self) -> Option<(u64, Option<Capture>)> {
        self.results
            .try_recv()
            .ok()
            .map(|(id, capture)| (id, capture.map(Capture::from)))
    }

    pub fn shutdown(&self) {
        let (lock, wake) = &*self.state;
        let mut state = lock.lock().unwrap();
        state.stopped = true;
        state.pending = None;
        wake.notify_one();
    }
}

impl Drop for CaptureWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use windows::Win32::Foundation::POINT;

    fn request(x: i32) -> CaptureRequest {
        CaptureRequest {
            point: POINT { x, y: 0 },
            method: RetrievalMethod::Auto,
            menu: false,
        }
    }

    #[test]
    fn worker_coalesces_pending_requests_and_stops_without_waiting_for_capture() {
        let state = Arc::new((Mutex::new(WorkerState::default()), Condvar::new()));
        let (sender, results) = mpsc::channel();
        let worker = CaptureWorker {
            state: Arc::clone(&state),
            results,
        };
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (notify_tx, notify_rx) = mpsc::channel();
        let thread = thread::spawn(move || {
            CaptureWorker::run_worker(
                state,
                sender,
                |request| {
                    started_tx.send(request.point.x).unwrap();
                    release_rx.recv().unwrap();
                    None
                },
                || {
                    notify_tx.send(()).unwrap();
                },
            );
        });
        let timeout = Duration::from_secs(2);
        assert!(worker.submit(1, request(1)));
        assert_eq!(started_rx.recv_timeout(timeout).unwrap(), 1);
        assert!(worker.submit(2, request(2)));
        assert!(worker.submit(3, request(3)));
        release_tx.send(()).unwrap();
        notify_rx.recv_timeout(timeout).unwrap();
        assert_eq!(worker.try_result().unwrap().0, 1);
        assert_eq!(started_rx.recv_timeout(timeout).unwrap(), 3);
        worker.shutdown();
        assert!(!worker.submit(4, request(4)));
        release_tx.send(()).unwrap();
        thread.join().unwrap();
        assert!(worker.try_result().is_none());
        assert!(notify_rx.try_recv().is_err());
    }
}
