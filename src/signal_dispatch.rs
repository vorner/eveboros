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
use std::mem::swap;

use mio::channel::{SyncSender, TrySendError};
use nix::c_int;
use nix::sys::signal::{Signal, NSIG, SigAction, SigHandler, SigSet, SIG_SETMASK, SA_NOCLDSTOP, SA_RESTART, sigaction};

const SIG_COUNT: usize = NSIG as usize;

pub type Flags = Arc<[AtomicBool; SIG_COUNT]>;

#[derive(Clone)]
pub struct SignalNotifier {
    pub sender: SyncSender<()>,
    pub flags: Flags,
}

struct SignalDispatch {
    registered: [Vec<SignalNotifier>; SIG_COUNT],
    setup: [bool; SIG_COUNT],
}

impl SignalDispatch {
    fn new() -> Self {
        SignalDispatch {
            registered: Default::default(),
            setup: [false; SIG_COUNT],
        }
    }
    fn register(&mut self, signal: Signal, notifier: SignalNotifier) {
        let sig_raw = signal as c_int;
        assert!(sig_raw < NSIG);
        let snum = sig_raw as usize;
        if !self.setup[snum] {
            let action = SigAction::new(SigHandler::Handler(handler), SA_NOCLDSTOP | SA_RESTART, SigSet::empty());
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

#[cfg(test)]
mod tests {
    use std::thread::spawn;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::sync_channel as chan;
    use std::sync::mpsc::TryRecvError;

    use nix::sys::signal::{Signal, raise};
    use mio::channel::sync_channel;

    use super::*;

    /// Start bunch of threads, each one registering for SIGUSR1 and wait for them.
    #[test]
    fn dispatch() {
        // Run this one multiple times, as there may be some race conditions
        for _ in 0..100 {
            // We need to know when the threads already registered
            let (s, r) = chan(1);
            let threads: Vec<_> = (0..10)
                .map(|_| {
                    let s = s.clone();
                    spawn(move || {
                        let flags: Flags = Default::default();
                        let (sender, receiver) = sync_channel(1);
                        register(Signal::SIGUSR1,
                                 SignalNotifier {
                                     sender: sender,
                                     flags: flags.clone(),
                                 });
                        // We registered, tell the main thread we are ready to get the signal
                        // (ignore the result, we don't care in tests)
                        let _ = s.send(());
                        loop {
                            match receiver.try_recv() {
                                Ok(_) => break,
                                Err(TryRecvError::Disconnected) => panic!(),
                                // We don't have blocking recv here, so we just loop here for a while
                                Err(TryRecvError::Empty) => (),
                            }
                        }
                        // Check the values
                        for (i, val) in flags.iter().enumerate() {
                            assert_eq!(i == Signal::SIGUSR1 as usize, val.load(Ordering::SeqCst));
                        }
                    })
                })
                .collect();
            // Wait for the threads to be ready
            for &_ in threads.iter() {
                let _ = r.recv();
            }
            // Send the signal
            raise(Signal::SIGUSR1).unwrap();
            // Wait for the threads and check they are happy
            for t in threads {
                t.join().unwrap();
            }
        }
    }
}
