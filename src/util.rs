//! Getting results back to the main loop, and keeping main-thread values there.

/// A channel to the main loop: what's sent, from any thread, is handed to the receiver's attached callback there.
#[allow(deprecated)] // (glib 0.18's; later versions replace it with async channels)
pub fn channel<T>() -> (glib::Sender<T>, glib::Receiver<T>) {
    glib::MainContext::channel(glib::Priority::DEFAULT)
}

/// Run work on a thread, then then(its result) on the main loop.
pub fn in_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static, then: impl FnOnce(T) + 'static) {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    let mut then = Some(then);
    rx.attach(None, move |v| {
        if let Some(f) = then.take() {
            f(v);
        }
        glib::ControlFlow::Break
    });
}

/// A value only ever touched on the thread that made it (the main thread): for the callbacks GIO wants Send + Sync
/// although it calls (and drops) them on the main loop. Getting or dropping it on another thread panics.
pub struct MainOnly<T>(std::mem::ManuallyDrop<T>, std::thread::ThreadId);

// SAFETY: the value is only reached through get() and Drop, which both check they're on the thread that made it.
unsafe impl<T> Send for MainOnly<T> {}
unsafe impl<T> Sync for MainOnly<T> {}

impl<T> MainOnly<T> {
    pub fn new(value: T) -> MainOnly<T> {
        MainOnly(std::mem::ManuallyDrop::new(value), std::thread::current().id())
    }

    pub fn get(&self) -> &T {
        assert_eq!(std::thread::current().id(), self.1, "used off the main thread");
        &self.0
    }
}

impl<T> Drop for MainOnly<T> {
    fn drop(&mut self) {
        assert_eq!(std::thread::current().id(), self.1, "dropped off the main thread");
        // SAFETY: dropped once, here.
        unsafe { std::mem::ManuallyDrop::drop(&mut self.0) };
    }
}
