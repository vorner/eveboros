extern crate eveboros;
extern crate nix;
extern crate libc;

/**
 * Tests for the signal and child handling. It doesn't really need to be an integration test, it's
 * one for here because of historical reasons. But testing something from outside doesn't hurt
 * either.
 */

use eveboros::{Loop, Event, Scope, Response, ChildExit, LoopIface, LoopIfaceObjSafe};
use nix::sys::signal::{raise, Signal};
use nix::unistd::{ForkResult, fork};
use libc::pid_t;
use std::process::exit;

/// Thing that terminates once the correct signal is received
struct SigRecipient(Signal);

impl Event<(), SigRecipient> for SigRecipient {
    fn init<S: Scope<(), SigRecipient>>(&mut self, scope: &mut S) -> Response {
        scope.signal(self.0);
        Ok(true)
    }
    fn signal<S: Scope<(), SigRecipient>>(&mut self, _scope: &mut S, signal: Signal) -> Response {
        assert_eq!(self.0, signal);
        Ok(false)
    }
}

/// Test signal delivery to events
#[test]
fn signal_test() {
    let mut l: Loop<(), SigRecipient> = Loop::new(()).unwrap();
    // Push bunch of signal recipients in
    let handle = l.insert(SigRecipient(Signal::SIGUSR1)).unwrap();
    for _ in 0..10 {
        l.insert(SigRecipient(Signal::SIGUSR2)).unwrap();
    }
    assert_eq!(11, l.event_count());
    // Sending SIGUSR2 will stop all the corresponding tasks
    raise(Signal::SIGUSR2).unwrap();
    while l.event_count() > 1 {
        l.run_one().unwrap();
    }
    // But the SIGUSR1 still stays
    assert!(l.event_alive(handle));
    raise(Signal::SIGUSR1).unwrap();
    l.run_until_complete(handle).unwrap();
}


/// Wait for a given child to terminate with exit code 42
struct ChildWatcher(pid_t);

impl<E: From<ChildWatcher>> Event<(), E> for ChildWatcher {
    fn init<S: Scope<(), E>>(&mut self, scope: &mut S) -> Response {
        scope.child(self.0);
        Ok(true)
    }
    fn child<S: Scope<(), E>>(&mut self, _scope: &mut S, pid: pid_t, exit: ChildExit) -> Response {
        assert_eq!(self.0, pid);
        assert_eq!(ChildExit::Exited(42), exit);
        Ok(false)
    }
}

fn fork_child() -> pid_t {
    match fork() {
        Ok(ForkResult::Child) => exit(42),
        Ok(ForkResult::Parent { child }) => child,
        Err(err) => panic!("Not enough forks: {}", err),
    }
}

#[test]
fn child_test() {
    let mut l: Loop<(), ChildWatcher> = Loop::new(()).unwrap();
    l.signal_enable(Signal::SIGCHLD);
    let pid = fork_child();
    let handle = l.insert(ChildWatcher(pid)).unwrap();
    l.run_until_complete(handle).unwrap();
}

/// Test we can't register the same PID twice
#[test]
#[should_panic]
fn child_multiple_test() {
    let mut l: Loop<(), ChildWatcher> = Loop::new(()).unwrap();
    l.signal_enable(Signal::SIGCHLD);
    let pid = fork_child();
    l.insert(ChildWatcher(pid)).unwrap();
    l.insert(ChildWatcher(pid)).unwrap();
}
