//! A module to have a global unix signal broadcasting
//!
//! If you want to know about signals, call the `register` function. Every time the concrete signal
//! happens, all corresponding receivers get woken up (the channel needs to have at least buffer of
//! size 1) and the flag of the signal is set.
//!
//! There's no way to unregister currently, only by dropping the receiver. Once another signal is
//! attempted to be sent, the sender is also dropped. Note that the signal handler is never
//! switched back by this module after the first registration.
use std::sync::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;
use std::iter::repeat;
use std::mem::swap;

use mio::channel::{SyncSender, TrySendError};
use nix::c_int;
use nix::sys::signal::{Signal, NSIG, SigAction, SigHandler, SigSet, SigFlags, SIG_SETMASK, SA_NOCLDSTOP, sigaction};

pub struct SignalNotifier {
    pub sender: SyncSender<()>,
    pub flags: Arc<[AtomicBool; NSIG as usize]>,
}

type SigNotifiers = [Vec<SignalNotifier>; NSIG as usize];

struct SignalDispatch {
    registered: SigNotifiers,
    setup: [bool; NSIG as usize],
}

impl SignalDispatch {
    fn new() -> Self {
        SignalDispatch {
            registered: <SigNotifiers>::default(),
            setup: [false; NSIG as usize],
        }
    }
    fn register(&mut self, signal: Signal, notifier: SignalNotifier) {
        let sig_raw = signal as c_int;
        assert!(sig_raw < NSIG);
        let snum = sig_raw as usize;
        if !self.setup[snum] {
            let action = SigAction::new(SigHandler::Handler(handler), SA_NOCLDSTOP, SigSet::empty());
            unsafe { sigaction(signal, &action).unwrap() };
            self.setup[snum] = true;
        }
        self.registered[snum].push(notifier);
    }
    fn dispatch(&mut self, signal: c_int) {
        assert!(signal < NSIG);
        let snum = signal as usize;
        if self.registered[snum].is_empty() {
            // Short-circuit all the following juggling
            return;
        }
        let mut tmp: Vec<SignalNotifier> = Vec::new();
        let ref mut signal = self.registered[signal as usize];
        // We swap the thing out, because rust doesn't let us do what we need inside that other
        // array
        swap(signal, &mut tmp);
        tmp = tmp.into_iter()
            .filter_map(|notifier| {
                // We first set the flag and then wake up the other thread. That way we are sure
                // the flag is set when it wakes up (if we wake it up first, it could do the check,
                // see the flag not being set and then go sleeping).
                // TODO: Maybe we could use weaker ordering? Does the sender actually take care of
                // proper synchronisation?
                notifier.flags[snum].store(true, Ordering::SeqCst);
                match notifier.sender.try_send(()) {
                    // The other side is gone, so remove this notifier
                    Err(TrySendError::Disconnected(_)) => None,
                    // It either went well, or we couldn't push a wakeup inside. But in that case,
                    // there's some wakeup already, so it will wake up anyway.
                    _ => Some(notifier),
                }
            })
            .collect();
        // Swap back what is left
        swap(signal, &mut tmp);
    }
}

lazy_static! {
    static ref SIGNAL_DISPATCH: Mutex<SignalDispatch> = Mutex::new(SignalDispatch::new());
}

extern "C" fn handler(signal: c_int) {
    SIGNAL_DISPATCH.lock().unwrap().dispatch(signal);
}

pub fn register(signal: Signal, notifier: SignalNotifier) {
    // We need to block all signals during the time we hold the mutex. Otherwise we could
    // get a deadlock.
    let block_all = SigSet::all();
    // We use unwrap as all the relevant errors returned mean a programmer error
    let old_mask = block_all.thread_swap_mask(SIG_SETMASK).unwrap();
    {
        // Change the internal data structures and register the signal if need be
        SIGNAL_DISPATCH.lock().unwrap().register(signal, notifier);
    }
    // Return the old signal mask
    old_mask.thread_set_mask().unwrap();
}
