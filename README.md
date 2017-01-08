# EveBoros ‒ a flexible event loop for Rust

[![Travis Build Status](https://api.travis-ci.org/vorner/eveboros.png?branch=master)](https://travis-ci.org/vorner/eveboros)
[![AppVeyor Build status](https://ci.appveyor.com/api/projects/status/c3qoud9df1dnwugu/branch/master?svg=true)](https://ci.appveyor.com/project/vorner/eveboros/branch/master)

Unlike [rotor](https://crates.io/crates/rotor) and
[Tokio](https://github.com/tokio-rs/tokio), EveBoros doesn't try to force you
into some kind of paradigm of using your events. It also resembles more
traditional events loop ‒ you register your callbacks (through Event objects)
and get them called.

It tries to be flexible and will support plugging futures and rotor machines in
the same event loop, allowing to use code from both project in addition to your
own on the top of the same loop.

It is in a very early development status. Currently only linux is supported.
Other unixes might come soon. Windows support might need pathes from someone
actually using Windows.
