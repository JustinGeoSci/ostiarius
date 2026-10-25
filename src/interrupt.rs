//  src/interrupt.rs

//! Ctrl-C is turned into a request that the run acts on, rather than an
//! immediate exit.
//!
//! By default Ctrl-C ends a Rust program at once with no cleanup, which in the
//! middle of an entry would leave the mutated file in place. The handler here
//! only records the request. The run notices it within one polling interval,
//! kills the command it is waiting on, restores the file, confirms its bytes,
//! and exits with code 130. Later presses are recorded the same way and cannot
//! cut the restore short. A run that must stop at once can still be killed
//! outright, and the next run then refuses to start until `--restore` has put
//! the saved copies back.

use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Installs the handler, once, before any file is mutated.
pub(crate) fn install() -> Result<(), ctrlc::Error> {
    ctrlc::set_handler(|| {
        if !REQUESTED.swap(true, Ordering::SeqCst) {
            eprintln!("interrupted: stopping the current command and restoring the file");
        }
    })
}

/// Whether Ctrl-C has been pressed.
pub(crate) fn requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}
