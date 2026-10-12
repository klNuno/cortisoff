# Contributing

Bug reports, plugin ideas and pull requests are welcome. For anything larger than a fix, open an issue first so the design can be agreed before the code is written.

## Building

You need a stable Rust toolchain. On Windows, the MSVC toolchain with the Visual Studio Build Tools. On Linux, the packages the CI installs:

```sh
sudo apt-get install pkg-config libssl-dev libgl1-mesa-dev libwayland-dev libx11-dev \
  libxcursor-dev libxi-dev libxkbcommon-dev libxrandr-dev \
  libxcb-render0-dev libxcb-shape0-dev libxcb-xfixes0-dev
```

Then:

```sh
cargo run -p cortisoff-app                       # the window, which starts a daemon if none runs
cargo run -p cortisoff-app -- --daemon --verbose # the daemon in the foreground with a live log
cargo run -p cortisoff-app -- --daemon --doctor  # what this machine can do, touches nothing
cargo run -p cortisoff-cli -- state              # the live state of a running daemon
```

A debug build never installs itself and never updates. Release builds do both, so run a release build from a scratch folder only if you mean to replace your installed copy.

## Checks

The CI runs these on Windows, Linux and macOS. Run them before you push:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The window's screenshot tests need a GPU, so the CI skips them. Run them locally when you change something visible, and attach the images to the pull request:

```sh
cargo test -p cortisoff-app --test capture -- --ignored   # every page, written to target/captures/ui
```

## How the code is laid out

| Crate | What it holds |
| --- | --- |
| `cortisoff-core` | The claim stack and the world model. No OS call, no clock, no thread, so it tests anywhere. |
| `cortisoff-runtime` | Finding plugins, the Lua sandbox they run in, the `cx` API of their daemon half, their settings, and installing them from a link. |
| `cortisoff-host` | Where the core meets a platform: the traits every platform implements. |
| `cortisoff-win`, `cortisoff-linux`, `cortisoff-mac`, `cortisoff-unix` | The platform halves: screens, input, windows, the pipe or socket. |
| `cortisoff-daemon` | The daemon loop, its journal and log, and the guard that restarts it after a crash. |
| `cortisoff-ipc` | The protocol between the daemon, the window and the command line. |
| `cortisoff-ui`, `cortisoff-app` | The window and the `ui` API plugins draw their pages with. |
| `cortisoff-cli` | The command line client. |
| `cortisoff-update` | Install, uninstall and signed self-update. |

Two rules hold the design together:

- Every change to the system goes through a claim with a priority. A plugin never sets a screen directly; it claims a capability, and the daemon applies the winning claim and restores the original value once nothing holds it. Values that outlive a crash, such as a refresh rate, are written to a journal before the first change.
- The window knows nothing about any plugin. Each plugin draws its own page, so a new plugin needs no change to the app.

## Writing a plugin

A plugin is a folder with a `plugin.toml` and an `init.lua`. Load it in place while you work on it; it reloads every time you save:

```sh
cortisoff-cli link path/to/my-plugin
cortisoff-cli trace     # everything the daemon sees and does, live
```

The full API is in [docs/plugins.md](docs/plugins.md), and the built-in plugins in [plugins/](plugins) are working examples.

## Pull requests

- One change per pull request, with a title that says what it does.
- Describe how you checked it, and add a screenshot or a short clip for anything visible.
- Do not bump the version; releases do that.

By contributing, you agree that your work is licensed under the same terms as the project, MIT or Apache-2.0 at the user's choice.
