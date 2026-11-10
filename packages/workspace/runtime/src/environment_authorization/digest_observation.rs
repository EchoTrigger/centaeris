//! Test-only observation of actual canonical bytes passed to digest operations.
use std::cell::RefCell;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct DigestWork {
    pub environment_calls: usize,
    pub environment_bytes: usize,
    pub profile_calls: usize,
    pub profile_bytes: usize,
}

thread_local! {
    static CURRENT: RefCell<Option<DigestWork>> = const { RefCell::new(None) };
}

pub(crate) fn measure<T>(operation: impl FnOnce() -> T) -> (T, DigestWork) {
    struct Restore(Option<DigestWork>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CURRENT.with(|current| current.replace(self.0));
        }
    }
    let previous = CURRENT.with(|current| current.replace(Some(DigestWork::default())));
    let _restore = Restore(previous);
    let result = operation();
    let work = CURRENT.with(|current| current.borrow_mut().take().expect("measurement is active"));
    (result, work)
}

pub(crate) fn environment_binding(bytes: usize) {
    CURRENT.with(|current| {
        if let Some(work) = current.borrow_mut().as_mut() {
            work.environment_calls += 1;
            work.environment_bytes += bytes;
        }
    });
}

pub(crate) fn environment_profile(bytes: usize) {
    CURRENT.with(|current| {
        if let Some(work) = current.borrow_mut().as_mut() {
            work.profile_calls += 1;
            work.profile_bytes += bytes;
        }
    });
}
