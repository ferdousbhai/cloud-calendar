//! Background jobs that change something: a save, a delete, an account being linked or removed.

use cloud_calendar_api as api;

/// Runs `job` off the main thread and hands its result to `done` on the main loop, through `omui::background`, which
/// holds the application until `done` has run: closing the last window would otherwise end the process under a save
/// or a delete. A job that panicked is an error like any other, said where the job's outcome is said.
pub fn write<T: Send + 'static>(
    job: impl FnOnce() -> api::Result<T> + Send + 'static,
    done: impl FnOnce(api::Result<T>) + 'static,
) {
    omui::background(job, move |result| {
        done(result.unwrap_or_else(|_| {
            Err(api::Error::new(
                api::ErrorKind::AccountUnavailable,
                omui::tr("failed unexpectedly"),
            ))
        }))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk::{gio, prelude::*};
    use std::cell::Cell;
    use std::rc::Rc;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    /// An application with no window ends as soon as it has been activated. With a write under way it ends only
    /// once the write has, and after its outcome was handed over.
    #[test]
    fn a_write_keeps_the_application_until_it_is_done() {
        let app = gio::Application::new(None, gio::ApplicationFlags::NON_UNIQUE);
        let (written, told) = (Arc::new(AtomicBool::new(false)), Rc::new(Cell::new(false)));
        app.connect_activate({
            let (written, told) = (written.clone(), told.clone());
            move |_| {
                let (written, told) = (written.clone(), told.clone());
                write(
                    move || {
                        std::thread::sleep(Duration::from_millis(300));
                        written.store(true, Ordering::SeqCst);
                        Ok(())
                    },
                    move |result| told.set(result.is_ok()),
                );
            }
        });
        app.run_with_args::<&str>(&[]);
        assert!(written.load(Ordering::SeqCst), "the application ended under the write");
        assert!(told.get(), "the write's outcome was never handed over");
    }
}
