//! Mux key handling: the keymap and its `AppAction`s. The `:` command
//! parser that used to live here moved into the `CommandSpec` registry
//! (`phi_builtins::MuxCommandsExtension`, context `Mux`); the mux dispatches
//! typed commands through its visual runtime.

mod keymap;

pub use keymap::*;
