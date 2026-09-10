use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExitReason {
    TrayQuit,
    FrontendQuit,
    ApplyUpdate,
    InstallUpdatesOnQuit,
}

struct ExitCoordinator {
    requested: AtomicBool,
}

impl ExitCoordinator {
    const fn new() -> Self {
        Self {
            requested: AtomicBool::new(false),
        }
    }

    fn request(&self) -> bool {
        self.requested
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
}

static NORMAL_EXIT: ExitCoordinator = ExitCoordinator::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExitStage {
    Requested,
    Dispatched,
    Reentry,
}

impl ExitStage {
    #[cfg(test)]
    const fn label(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Dispatched => "dispatched",
            Self::Reentry => "reentry",
        }
    }
}

fn request_once_with(
    coordinator: &ExitCoordinator,
    reason: ExitReason,
    dispatch: impl FnOnce(),
) -> bool {
    request_once_with_observer(coordinator, reason, dispatch, |stage, reason| match stage {
        ExitStage::Requested => tracing::debug!(?reason, "normal exit requested"),
        ExitStage::Dispatched => tracing::debug!(?reason, "normal exit dispatched"),
        ExitStage::Reentry => tracing::debug!(?reason, "normal exit request ignored as reentry"),
    })
}

fn request_once_with_observer(
    coordinator: &ExitCoordinator,
    reason: ExitReason,
    dispatch: impl FnOnce(),
    mut observe: impl FnMut(ExitStage, ExitReason),
) -> bool {
    let first = coordinator.request();
    if first {
        observe(ExitStage::Requested, reason);
        dispatch();
        observe(ExitStage::Dispatched, reason);
    } else {
        observe(ExitStage::Reentry, reason);
    }
    first
}

pub(crate) fn request_normal_exit(app: &tauri::AppHandle, reason: ExitReason) -> bool {
    request_once_with(&NORMAL_EXIT, reason, || {
        crate::geometry_store::flush_pending();
        app.exit(0);
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn first_request_dispatches_and_second_is_a_reentry() {
        let coordinator = ExitCoordinator::new();
        let dispatches = AtomicUsize::new(0);

        assert!(request_once_with(
            &coordinator,
            ExitReason::FrontendQuit,
            || {
                dispatches.fetch_add(1, Ordering::SeqCst);
            },
        ));
        assert!(!request_once_with(
            &coordinator,
            ExitReason::TrayQuit,
            || {
                dispatches.fetch_add(1, Ordering::SeqCst);
            },
        ));
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_requests_have_exactly_one_first_dispatch() {
        let coordinator = Arc::new(ExitCoordinator::new());
        let dispatches = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();

        for _ in 0..16 {
            let coordinator = Arc::clone(&coordinator);
            let dispatches = Arc::clone(&dispatches);
            threads.push(std::thread::spawn(move || {
                request_once_with(&coordinator, ExitReason::ApplyUpdate, || {
                    dispatches.fetch_add(1, Ordering::SeqCst);
                })
            }));
        }

        let first_count = threads
            .into_iter()
            .map(|thread| thread.join().expect("exit request thread"))
            .filter(|first| *first)
            .count();
        assert_eq!(first_count, 1);
        assert_eq!(dispatches.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn dispatched_stage_is_reported_after_dispatch_returns_with_fixed_reason() {
        let coordinator = ExitCoordinator::new();
        let events = RefCell::new(Vec::new());

        assert!(request_once_with_observer(
            &coordinator,
            ExitReason::FrontendQuit,
            || events.borrow_mut().push((None, "dispatch")),
            |stage, reason| events.borrow_mut().push((Some(reason), stage.label())),
        ));

        assert_eq!(
            events.into_inner(),
            vec![
                (Some(ExitReason::FrontendQuit), "requested"),
                (None, "dispatch"),
                (Some(ExitReason::FrontendQuit), "dispatched"),
            ]
        );
    }
}
