//! Run bunch of commands in parallel
//!
//! This program reads commands from stdin (one command per line) and executes them in parallel. By
//! default at most 4 commands run concurrently, but the limit can be changed on command line.

extern crate eveboros;

use std::env::args;
use std::io::{Write, BufRead, stderr};

use eveboros::error::{Error, Result};
use eveboros::{Loop, Event, Response, Scope, LoopIfaceObjSafe, LoopIface, ChildExit, pid_t};

// Get the limit of number of parallel jobs
fn num_jobs() -> Result<usize> {
    args()
        .nth(1)
        .or(Some("4".into()))
        .unwrap()
        .parse()
        .map_err(|e| Error::UserStr(format!("Wrong number of jobs: {}", e)))
}

struct Context<'a> {
    errors: bool,
    commands: std::io::Lines<std::io::StdinLock<'a>>,
}

// Keeps one command alive. Once the command terminates, next one is launched (or the event
// terminates, if there are no more commands).
struct Runner(Option<String>);

impl Runner {
    fn run<'a, Ev, S: Scope<Context<'a>, Ev>>(&mut self, scope: &mut S) -> Response {
        match scope.with_context(|ctx| Ok(ctx.commands.next()))? {
            None => Ok(false), // No more commands to run, terminate
            Some(Err(e)) => Err(e.into()), // Propagate the error from stdin
            Some(Ok(cmd)) => {
                // We have a process to start, do so and register it
                let child = std::process::Command::new("sh").arg("-c").arg(&cmd).spawn()?;
                scope.child(child.id() as pid_t)?;
                self.0 = Some(cmd);
                Ok(true)
            },
        }
    }
}

impl<'a, Ev> Event<Context<'a>, Ev> for Runner {
    fn init<S: Scope<Context<'a>, Ev>>(&mut self, scope: &mut S) -> Response {
        // Run one task at start
        self.run(scope)
    }
    fn child<S: Scope<Context<'a>, Ev>>(&mut self, scope: &mut S, _pid: pid_t, exit: ChildExit) -> Response {
        // Print how well it went
        // TODO: There just must be a more elegant way than this
        let cmd = self.0.take().unwrap();
        let err = match exit {
            ChildExit::Exited(0) => None, // That's OK
            ChildExit::Exited(e) => Some(format!("Command {} exited with err code {}", cmd, e)),
            ChildExit::Signaled(s) => Some(format!("Command {} was killed by signal {}", cmd, s as i32)),
        };
        if let Some(e) = err {
            writeln!(stderr(), "{}", e).unwrap();
            scope.with_context(|ctx| {
                    ctx.errors = true;
                    Ok(())
                })?;
        }
        // Run another task
        self.run(scope)
    }
}

fn run() -> Result<()> {
    let jobs = num_jobs()?;
    let stdin = std::io::stdin();
    // Create the loop
    let mut l: Loop<_, Runner> = Loop::new(Context {
        errors: false,
        commands: stdin.lock().lines(),
    })?;
    // Insert runners, each one keeping one command alive
    for _ in 0..jobs {
        l.insert(Runner(None))?;
    }
    // Wait for all the runners to finish
    l.run_until_empty()?;
    l.with_context(|ctx| if ctx.errors {
        Err(Error::UserStr("A command failed".into()))
    } else {
        Ok(())
    })
}

fn main() {
    if let Err(e) = run() {
        writeln!(stderr(), "{}", e).unwrap();
        std::process::exit(1);
    }
}
