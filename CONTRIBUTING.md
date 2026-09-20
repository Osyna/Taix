# Contributing

Thanks for taking the time. Contributions of any size are welcome, including
typo fixes and questions that turn into documentation.

## Before you start

For anything more than a small fix, open an issue first and describe what you
want to change. It saves you from building something that turns out not to fit.

## Setting up

TaiX needs a `tmux` on `PATH` at runtime. Building the CLI and the terminal UI
needs nothing but a Rust toolchain; the desktop window additionally needs GTK 4,
libadwaita and WebKitGTK 6.

```bash
git clone https://github.com/Osyna/Taix.git
cd Taix
cargo build --release                      # taix: the CLI and the TUI
cargo build --release -p taix --features gui   # taix-gui: the desktop window
```

`cargo build` deliberately does not build `taix-gui`, so the workspace compiles
on a machine with no GTK at all.

## Running the tests

```bash
cargo test --workspace                          # needs the GTK dev packages
cargo test                                      # without them: everything but the GUI
cargo clippy --workspace --all-targets
cargo clippy -p taix --features gui --all-targets
cargo fmt --all --check
```

CI runs the full set on every push. Clippy is clean at the time of writing,
so a new warning is yours.

Tests run as threads of one process, so anything reading `TZ` or an `XDG_*`
path has to take `taix_core::testenv::lock` (or `redirect`) first. Two tests
that skipped it were failing about one run in five on Ubuntu.

## Making a change

1. Branch off `main`.
2. Keep the change focused: one concern per pull request.
3. Match the style already in the file you are editing.
4. Update the README or docs if you changed behaviour anyone depends on.
5. Write a commit message that explains *why*, not just what.

## Pull requests

Describe what changed and how you verified it. Screenshots help for anything
visual. CI has to pass before review.

## Reporting bugs

Include what you did, what happened, what you expected, and the versions of
TaiX, your OS and your runtime. A minimal reproduction is worth more than a
long description.

## Code of conduct

By participating you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
