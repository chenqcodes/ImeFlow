//! Read-only tray observation. This module never requests focus checks or IME writes.
use crate::{
    feedback::FeedbackBus,
    ime::{ImeController, LayerObservation},
    process::{self, InputTarget},
    watcher::AppWatcher,
};
use std::{cell::RefCell, sync::mpsc};
use windows::Win32::Foundation::HWND;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Request {
    target: InputTarget,
    revision: u64,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    request: Request,
    observation: LayerObservation,
}

#[derive(Default)]
struct StableReading {
    previous: Option<Sample>,
}
impl StableReading {
    // Two matching reads suppress transitional conversion flags without using a key as proof.
    fn accept(&mut self, sample: Sample) -> bool {
        let stable = self.previous == Some(sample);
        self.previous = Some(sample);
        stable
    }
}

struct Observer {
    send: mpsc::Sender<Request>,
    receive: mpsc::Receiver<Option<Sample>>,
    pending: bool,
    reading: StableReading,
}
thread_local! { static OBSERVER: RefCell<Option<Observer>> = const { RefCell::new(None) }; }

pub fn start() -> Result<(), String> {
    let (send, requests) = mpsc::channel::<Request>();
    let (results, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name("ime-status-reader".into())
        .spawn(move || {
            // COM initialization is per-thread; tsf's legacy process-level guard is insufficient here.
            use windows::Win32::System::Com::{
                CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
            };
            let com_initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
            while let Ok(request) = requests.recv() {
                let sample = if request.target.ensure_current().is_ok() {
                    let snapshot = ImeController::read(HWND(request.target.root as *mut _));
                    request.target.ensure_current().is_ok().then_some(Sample {
                        request,
                        observation: snapshot.observe_layer(),
                    })
                } else {
                    None
                };
                if results.send(sample).is_err() {
                    break;
                }
            }
            if com_initialized {
                unsafe {
                    CoUninitialize();
                }
            }
        })
        .map_err(|e| e.to_string())?;
    OBSERVER.with(|slot| {
        *slot.borrow_mut() = Some(Observer {
            send,
            receive,
            pending: false,
            reading: StableReading::default(),
        })
    });
    Ok(())
}

pub fn stop() {
    // Drop both channels; an in-flight bounded OS read can finish without blocking the UI.
    OBSERVER.with(|slot| {
        slot.borrow_mut().take();
    });
}

pub fn poll(bus: &FeedbackBus) {
    let root = process::root_hwnd(process::foreground_hwnd());
    // The settings/tray window is not the input context the user wants to observe.
    if process::hwnd_process_id(root) == std::process::id() {
        return;
    }
    let target = InputTarget::capture(root).ok();
    let revision = bus.observation_revision();
    let generation = AppWatcher::generation();
    let mut changed = false;
    OBSERVER.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(observer) = slot.as_mut() else { return; };
        if observer.pending {
            match observer.receive.try_recv() {
                Ok(sample) => {
                    observer.pending = false;
                    let valid = sample.filter(|s| Some(s.request.target) == target && s.request.revision == revision && s.request.generation == generation);
                    if let Some(sample) = valid {
                        if observer.reading.accept(sample) {
                            changed = bus.record_observation(revision, sample.observation);
                            if changed { tracing::debug!(observation = ?sample.observation, "tray live observation; no IME action"); }
                        }
                    } else { observer.reading.previous = None; }
                }
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    observer.pending = false;
                    observer.reading.previous = None;
                    changed = bus.record_observation(revision, LayerObservation::Unknown);
                    return;
                }
            }
        }
        if let Some(target) = target {
            observer.pending = observer.send.send(Request { target, revision, generation }).is_ok();
        } else {
            observer.reading.previous = None;
            changed |= bus.record_observation(revision, LayerObservation::Unknown);
        }
    });
    if changed {
        crate::tray::refresh();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mode::TargetMode;
    fn sample() -> Sample {
        Sample {
            request: Request {
                target: InputTarget {
                    root: 1,
                    focus: 2,
                    thread: 3,
                },
                revision: 4,
                generation: 5,
            },
            observation: LayerObservation::Confident(TargetMode::ImeEnglish),
        }
    }
    #[test]
    fn manual_mode_change_is_accepted_only_after_stable_reads() {
        let mut reader = StableReading::default();
        let english = sample();
        assert!(!reader.accept(english));
        assert!(reader.accept(english));
        let chinese = Sample {
            observation: LayerObservation::Confident(TargetMode::NativeChinese),
            ..english
        };
        assert!(!reader.accept(chinese));
        assert!(reader.accept(chinese));
        assert!(!reader.accept(english));
        assert!(reader.accept(english));
    }
    #[test]
    fn focus_write_and_confidence_changes_require_fresh_confirmation() {
        let mut reader = StableReading::default();
        let mut s = sample();
        reader.accept(s);
        s.request.revision += 1;
        assert!(!reader.accept(s));
        s.request.generation += 1;
        assert!(!reader.accept(s));
        s.request.target.focus += 1;
        assert!(!reader.accept(s));
        s.observation = LayerObservation::Probable(TargetMode::ImeEnglish);
        assert!(!reader.accept(s));
        assert!(reader.accept(s));
    }
}
