# Writing a plugin

A cortisoff plugin is a folder with a `plugin.toml` and a Lua file, `init.lua` by default. The Lua runs on LuaJIT in a sandbox. The file returns one table, and that table has two halves:

- The daemon half (`start`, `on`, `stop`) reacts to events and timers and changes the machine through `cx`.
- The window half (`status`, `page`) draws the plugin's card and settings page through `ui`.

The two halves run in separate Lua states, in separate processes. They share only the settings table `s`, the state the daemon half publishes with `cx:publish`, and the messages the page sends with `ui:send`. A local variable set in `start` is not visible from `page`.

The four built-in plugins in [`plugins/`](../plugins) are complete examples. `plugins/night-light/init.lua` uses most of the API.

## A minimal plugin

`plugin.toml`:

```toml
id = "warm-reader"
name = "Warm reader"
version = "0.1.0"
description = "Warms the screen while a reader app is in front."
platforms = ["windows", "linux", "macos"]
```

`init.lua`:

```lua
local plugin = { settings = { app = "sumatrapdf.exe", kelvin = 4000 } }

local function apply(cx, s)
  local app = cx:world().app
  if app and app.exe:lower() == s.app:lower() then
    local ok, err = cx:claim("display.color_temp", s.kelvin, { fade = 800 })
    if not ok then cx:warn(err) end
  else
    cx:release("display.color_temp", { fade = 800 })
  end
end

function plugin.start(cx, s)
  cx:watch("app.focus")
  apply(cx, s)
end

function plugin.on(cx, s, ev)
  apply(cx, s)
end

function plugin.page(ui, s)
  ui:card("Warm reader", function(ui)
    ui:row("App", "The program's file name.", function(ui) s.app = ui:text(s.app) end)
    ui:row("Temperature", nil, function(ui)
      s.kelvin = ui:slider(s.kelvin, { min = 1900, max = 6500, step = 100, track = "kelvin" })
    end)
  end)
end

return plugin
```

Run it from its folder while cortisoff is running:

```sh
cortisoff-cli link path/to/warm-reader
```

Linking loads the folder in place and switches the plugin on. You can also paste the folder path into the window's Add a plugin page. A linked plugin reloads every time you save a `.lua` or `.toml` file in it, up to three folder levels deep, in both the daemon and the window.

Watch what it does:

```sh
cortisoff-cli trace
```

`trace` prints every event the daemon receives (`event ...`), every change it applies (`action ...`), log lines (`Info Warm reader: ...`, `Warn ...`, `Error ...`) and plugin state changes (`plugin warm-reader running`, or `stopped: <error>`). Anything you `print` or pass to `cx:log` shows up there. A Lua error stops that one call, shows on the plugin's page in the window and in `trace`; the plugin keeps running.

Other useful commands while you work:

```sh
cortisoff-cli plugins                          # every plugin, on or off
cortisoff-cli settings warm-reader             # print its settings
cortisoff-cli settings warm-reader '{"kelvin": 3500}'
cortisoff-cli send warm-reader pause '{"minutes": 5}'
cortisoff-cli state                            # the world, every plugin, who holds what
cortisoff-cli doctor                           # which capabilities this machine has, and why not
cortisoff-cli remove warm-reader               # unlink the folder
```

## `plugin.toml`

| Field | Required | Meaning |
| --- | --- | --- |
| `id` | yes | 1 to 64 characters: lowercase letters, digits, `-` and `_`. Names the installed folder and the key the settings are stored under. A plugin with the same id as another one replaces it (see [Installing and sharing](#installing-and-sharing)). |
| `name` | yes | What the window shows. Must not be empty. Also prefixes the plugin's log lines. |
| `version` | yes | Free text, shown in the window. A changed version makes cortisoff restart the plugin on the next rescan. |
| `platforms` | yes | At least one of `"windows"`, `"linux"`, `"macos"`. On any other system the plugin is listed but does not start, and says what it was made for. cortisoff never guesses this for you. |
| `description` | no | One line, shown in the window. |
| `icon` | no | An icon name from the list under [`ui:button`](#ui-reference-for-pages). An unknown name or none shows the plugin icon. |
| `author` | no | Shown in the window. |
| `entry` | no | The Lua file to run, relative to the folder. Default `init.lua`. Must stay inside the folder. |
| `ffi` | no | `true` asks for LuaJIT's FFI, which removes the sandbox. Default `false`. See [Sandbox](#sandbox). |

## Lifecycle

`init.lua` runs once per Lua state and must return a table. cortisoff looks for these fields on it; all are optional.

| Field | Called by | When |
| --- | --- | --- |
| `plugin.settings` | both | A table of defaults, read once after the file runs. |
| `plugin.start(cx, s)` | daemon | The plugin is switched on, cortisoff starts, or the folder was reloaded. |
| `plugin.on(cx, s, ev)` | daemon | A watched event without its own callback, a hotkey without its own callback, new settings from the window or the CLI, or a message from the page. |
| `plugin.stop(cx)` | daemon | The plugin is switched off, removed, reloaded, or cortisoff exits. It also receives `s` as a second argument. |
| `plugin.status(s, st)` | window | Whenever the window draws the plugin's card. Returns one table. |
| `plugin.page(ui, s, st)` | window | Every frame while the plugin's page is open. |

`s` is the settings table: `plugin.settings` with whatever the user changed laid over it, key by key, nested tables included. The daemon half gets the same table on every call, so a reference you keep stays current; when the settings change from outside it is refilled in place and `on` receives `{ kind = "settings" }`. Both halves may change `s`. After each call cortisoff compares it with what it had and saves the difference. Only values that differ from your defaults are stored, so a new default in a new version reaches every user who never touched that key. Settings must be plain data: functions in `s` are skipped when saving. In the page, an empty table is saved as an empty list.

`st` is whatever the daemon half last passed to `cx:publish`, or `nil` before the first publish. Use it to show live state in the window.

`status` returns `{ line = "...", detail = "...", state = "holding" | "waiting" | "paused" | "failed" }`. `line` is the short text on the card, `detail` the second line, `state` sets the colour. A missing `state` is `"waiting"`.

`page` gets a fresh copy of `s` each frame. Edit it in place, as the controls return their new value; the difference goes back to the daemon. Keep scratch values such as a half-typed text field in a local outside the function: locals of the module live as long as the page.

Stopping releases everything the plugin claimed, cancels its timers, hotkeys, watches and pending HTTP answers. When the user switches it off the screen fades back over 600 ms; on a reload or when cortisoff exits it goes back at once.

Every call into a plugin, in either half, has two seconds to return. A call that runs longer is stopped with an error and the plugin keeps running.

## Events

Subscribe with `cx:watch(kind)`. Each event is a table whose `kind` is the watched kind. Events are delivered to the callback given to `cx:watch`, or to `plugin.on` when there is none. A kind nobody watches is never hooked, so watching costs nothing until you ask.

| Kind | Fields | Meaning |
| --- | --- | --- |
| `app.focus` | `exe`, `path`, `title` (may be `nil`), `pid` | Another application came to the front. `exe` is its file name, lowercase on Windows (`cs2.exe`, `firefox`); `path` is the full path. |
| `app.fullscreen` | `exclusive` | An exclusive-fullscreen game took or released the screen. Narrower than `app.maximized`. |
| `app.maximized` | `maximized` | The focused window fills its monitor (maximised, borderless or exclusive fullscreen), or stopped doing so. |
| `user.idle` | `idle`, `seconds` | `idle = true` when an inactivity threshold is crossed, with that threshold in `seconds`; `idle = false, seconds = 0` when the user comes back. |
| `session.lock` | `locked` | The session was locked or unlocked. |
| `power.source` | `on_battery` | The machine moved between battery and mains. |
| `audio.active` | `active` | Sound started or stopped playing. |
| `display.config` | none | Monitors were plugged, unplugged or rearranged. |
| `clock` | `minute`, `second`, `ms` | The wall clock moved under the timers: resume from sleep, a time zone or daylight saving change. It is not sent every minute; use `cx:at` or `cx:every` for that. |

For `user.idle`, pass the threshold in seconds: `cx:watch("user.idle", { after = 600 })`. That watch only hears `idle = true` for its own threshold, plus every `idle = false`. Several watches with different thresholds can coexist.

Not every kind exists on every system; see the platform column in the next section. Watching a missing kind is not an error, the event simply never comes.

`plugin.on` and callbacks also receive these, which you do not watch:

| `kind` | Fields | Sent when |
| --- | --- | --- |
| `settings` | none | Settings changed from the window or the CLI. Only to `plugin.on`. |
| `message` | `name`, `data` | The page called `ui:send(name, data)`, or someone ran `cortisoff-cli send <id> <name> [json]`. Only to `plugin.on`. |
| `timer` | `id` | A timer fired. Only to its callback. |
| `hotkey` | `id`, `chord` | A hotkey was pressed. To its callback, else `plugin.on`. |
| `http` | `id`, `ok`, `status`, `body`, `error` | An HTTP request finished. Only to its callback. |

Every callback has the signature `function(cx, s, ev)`.

## Capabilities and claims

A capability is something on the machine that holds a value, such as the colour temperature. A plugin never sets it directly. It claims a value with `cx:claim(cap, value, opts)`, and cortisoff decides who wins:

- Each plugin holds at most one claim per capability. Claiming again replaces your own claim.
- The highest priority wins. On equal priority the most recent claim wins.
- When the winner releases, the next claim underneath takes over. With no claim left, the capability goes back to the value it had before cortisoff first changed it, captured and kept on disk so it is put back even after a crash.
- A manual override from `cortisoff-cli set` or the window outranks every plugin. A slider being dragged with `preview` outranks that.

Priorities are integers, clamped to -100 through 999. The anchors:

| Priority | Use |
| --- | --- |
| -100 | A default that should lose to everything. |
| 0 | The default when you name none. |
| 100 | A plugin reacting to the focused application, which should beat a schedule. |

`opts.fade` is a duration in milliseconds, up to a day. With a fade, a numeric value travels from where it is now to the new value; cortisoff steps it, you claim once. Claiming the same target again while it fades does not restart the fade. Values that are not numbers switch at once. `cx:release(cap, { fade = ms })` fades back to whatever lies underneath.

`cx:claim` returns `true` when the claim was accepted, or `nil` and a sentence when the capability is missing on this machine, only watchable, or the value has the wrong type. Accepted does not mean winning: the claim is applied after your call returns. `cortisoff-cli state` shows who holds what.

Capabilities you can claim:

| Id | Value | Windows | Linux | macOS |
| --- | --- | --- | --- | --- |
| `display.color_temp` | Integer kelvin, 1500 to 6600. 6600 leaves the screen as it is. Fades. | yes | yes (X11) | yes |
| `display.vibrance` | Integer, the driver's range (0 to 100 when it reports none). Fades. | NVIDIA only; AMD is not written yet | no | no |
| `display.mode` | `{ width = 2560, height = 1440, refresh = 240 }`. `refresh` is in Hz and may be fractional (59.94). Leave out `width` and `height` to change only the rate, leave out `refresh` to change only the resolution. Switches at once. | yes | yes (X11) | yes |
| `wallpaper.playback` | `true` plays, `false` pauses. Switches at once. | with Wallpaper Engine or Lively installed | no | no |

Watch-only capabilities, the event kinds above:

| Id | Windows | Linux | macOS |
| --- | --- | --- | --- |
| `app.focus`, `app.maximized`, `app.fullscreen` | yes | X11 with a window manager that publishes `_NET_ACTIVE_WINDOW` | yes |
| `user.idle` | yes | X11 with MIT-SCREEN-SAVER; not under XWayland | yes |
| `session.lock` | yes | no | yes |
| `power.source` | yes | when `/sys/class/power_supply` exists | when IOKit reports a power source |
| `audio.active` | yes | when ALSA is present | no |
| `display.config` | yes | X11 with RandR 1.2 | yes |

Availability is probed when cortisoff starts, not decided at build time. Check with `cx:has(cap)` before relying on one, and run `cortisoff-cli doctor` to read why one is missing.

A value given to `cx:claim` converts as follows: a boolean stays a boolean, a whole number becomes an integer (so `3200.0` from arithmetic is `3200`), another number stays a float, a string stays text, and a table with `width`, `height` or `refresh` is a display mode.

## `cx` reference

`cx` is passed to every daemon call and is valid only during that call. Methods that can fail for a reason outside your control return `true`, or `nil` and a sentence, like Lua's `io` functions. Methods that fail because of a wrong argument raise an error.

| Method | Returns | Notes |
| --- | --- | --- |
| `cx:id()` | string | The plugin id. |
| `cx:platform()` | string | `"windows"`, `"linux"` or `"macos"`. |
| `cx:watch(kind, [fn], [opts])` | nothing | Subscribe to an event kind. `cx:watch(kind, opts)` without a function also works. `opts.after` is for `user.idle` only, in seconds, at least 1. An unknown kind raises an error. |
| `cx:unwatch(kind)` | nothing | Drops every watch of that kind. |
| `cx:every(ms, fn)` | timer id | Repeats every `ms`, at least 50. First call after `ms`. |
| `cx:after(ms, fn)` | timer id | Once, after `ms`. |
| `cx:at(time, fn)` | timer id | Every day at `time`: `"20:30"`, `"7:05"`, or a minute of the day (`1230`). Measured again after sleep or a clock change. Raises an error if the system gives no clock. |
| `cx:cancel(id)` | nothing | Cancels one of your timers. Unknown ids are ignored. |
| `cx:now()` | minute, ms | The local minute of the day with its fraction (`nil` without a clock), and the monotonic milliseconds timers count in. |
| `cx:claim(cap, value, [opts])` | `true` or `nil, err` | `opts.priority` (default 0, clamped to -100 through 999), `opts.fade` in ms (0 to 86 400 000). See [Capabilities and claims](#capabilities-and-claims). |
| `cx:release(cap, [opts])` | nothing | Lets go of one capability. `opts.fade` in ms. |
| `cx:release_all([opts])` | nothing | Lets go of everything you hold. `opts.fade` in ms. |
| `cx:has(cap)` | boolean | Whether this machine has the capability, settable or not. |
| `cx:rest(cap)` | value or `nil` | What the capability shows when nobody holds it, in a claim's shape, for example `6600` for `display.color_temp`. `nil` where there is no such number. |
| `cx:world()` | table | The current state: `app` (`{ exe, path, title, pid }` as in `app.focus`, or `nil`), `fullscreen`, `maximized`, `idle`, `idle_seconds`, `locked`, `on_battery`, `audio`. |
| `cx:publish(state)` | nothing | Sends `state` (plain data) to the window, where it arrives as `st`. |
| `cx:log(...)` | nothing | An info line in the log, arguments joined with spaces. `print` does the same, joined with tabs, up to 256 lines per call. |
| `cx:warn(...)` | nothing | A warning line in the log. |
| `cx:run(program, ...)` | nothing | Starts a program with the given arguments, after the call returns. An empty program raises an error. |
| `cx:hotkey(chord, [fn])` | id, or `nil, err` | A global hotkey. Fails when the chord does not parse or another plugin holds it. If the system refuses it later (another application owns it), the plugin shows that as its error. |
| `cx:unhotkey(id)` | nothing | Drops one of your hotkeys. |
| `cx:send_keys(chord)` | `true` or `nil, err` | Presses and releases a chord in the focused window. |
| `cx:type(text)` | `true` or `nil, err` | Types text into the focused window. |
| `cx:notify(title, [body])` | `true` or `nil, err` | A desktop notification. On Linux it needs `notify-send`. |
| `cx:clipboard()` | text, or `nil, err` | The clipboard as plain text. |
| `cx:set_clipboard(text)` | `true` or `nil, err` | Puts text on the clipboard. |
| `cx:windows()` | list | Visible top-level windows, front to back: `{ id, title, exe, path, pid, focused }`, `exe` and `path` as in `app.focus`. |
| `cx:window(id, action)` | `true` or `nil, err` | `action` is `"activate"` (or `"focus"`), `"minimize"`, `"maximize"`, `"restore"` or `"close"`. |
| `cx:http(url, [opts], fn)` | request id | Runs an HTTP request on its own thread. `url` must start with `http://` or `https://`. `opts.method` (default `"GET"`), `opts.headers` (a table of strings), `opts.body` (string), `opts.timeout` in ms (default 15 000, clamped to 500 through 120 000). `fn(cx, s, answer)` receives `{ kind = "http", id, ok, status, body, error }`; `ok` is true for a 2xx answer with no error, `status` is 0 when the request failed. The body is cut off at 4 MB. |

Key chords are written `ctrl+alt+n`, `shift+f5`, `win+space`, case-insensitive, the last part being the key. Modifiers: `ctrl` (`control`), `alt` (`option`), `shift`, `win` (`meta`, `super`, `cmd`, `command`). Keys: a letter or digit, `f1` to `f24`, `space`, `enter`, `tab`, `esc`, `backspace`, `delete`, `insert`, `home`, `end`, `pageup`, `pagedown`, `up`, `down`, `left`, `right`, `printscreen`, `pause`, `volume_up`, `volume_down`, `volume_mute`, `media_next`, `media_previous`, `media_play_pause`, `media_stop`.

On Linux, hotkeys, keys, text, the clipboard and windows need an X11 display.

## `ui` reference for pages

`ui` is passed to `page` and to the functions given to containers. Containers take a function that receives a nested `ui`. Controls take the current value and return the new one, so you write `s.x = ui:switch(s.x)`.

| Method | Returns | Notes |
| --- | --- | --- |
| `ui:card(title, fn)` | nothing | A card. `title` may be `nil`. |
| `ui:row(label, hint, fn)` | nothing | A settings row: label and optional hint on the left, `fn` draws the control on the right. |
| `ui:horizontal(fn)` | nothing | Lays children out left to right. |
| `ui:wrap(fn)` | nothing | Left to right, wrapping onto new lines. |
| `ui:vertical(fn, [gap])` | nothing | Top to bottom, `gap` pixels between items. |
| `ui:right(fn)` | nothing | Aligns children to the right edge. |
| `ui:label(text, [style])` | nothing | `style`: `size` (default 14), `strong`, `mono`, `color`. |
| `ui:hint(text)` | nothing | Muted small text. |
| `ui:space([px])` | nothing | Empty space, 8 px by default. |
| `ui:divider()` | nothing | A thin line, between rows. |
| `ui:width()` | number | The width left in the current container. |
| `ui:switch(on)` | boolean | An on/off switch. |
| `ui:button(text, [icon])` | boolean | `true` on the frame it was clicked. |
| `ui:chip(text, [icon])` | boolean | A small pill button, `true` when clicked. |
| `ui:slider(value, opts)` | number | `opts.min` (default 0), `opts.max` (default `min + 100`), `opts.step` (default 1), `opts.track = "kelvin"` for a colour temperature track, `opts.text = function(v) return "..." end` for the readout, `opts.preview = cap` to show the value live while dragging. |
| `ui:time(minute)` | number | A time of day field, `HH:MM`, holding a minute of the day. Click to type, drag to move by five minutes. |
| `ui:text(value, [opts])` | string | A one-line text field. `opts.hint` (placeholder), `opts.width` (default 160, 0 for the full width), `opts.mono`. |
| `ui:select(value, options, [labels])` | the chosen option | A dropdown over the list `options`. `labels` maps an option to its label, or is a list in the same order. |
| `ui:send(name, [data])` | nothing | Sends `{ kind = "message", name, data }` to the daemon half's `plugin.on`. |
| `ui:canvas(width, height, fn)` | nothing | A painting area. `width` may be `nil` for the full width. `fn(p)` receives a painter whose coordinates start at the canvas' top-left corner. |

A slider with `preview = "display.color_temp"` claims that capability at a priority above everything while it is dragged, and fades back over 800 ms when it is let go, so the user sees the value before keeping it.

Icon names for `ui:button`, `ui:chip` and the manifest's `icon`: `home`, `settings`, `add`, `moon`, `monitor`, `picture`, `bolt`, `pause`, `close`, `clock`, `game`, `volume`, `lock`, `fullscreen`, `arrow`, `plugin`, `check`, `keyboard`, `play`, `delete`, `edit`, `search`, `warning`, `refresh`, `sun`, `power`, `battery`.

The canvas painter `p`:

| Method | Notes |
| --- | --- |
| `p:size()` | Returns width, height. |
| `p:line(points, [width], color)` | A polyline. `points` is flat: `{ x1, y1, x2, y2, ... }`. |
| `p:vline(x, top, bottom, [width], color)` | A vertical line. |
| `p:hline(left, right, y, [width], color)` | A horizontal line. |
| `p:rect(x, y, w, h, color, [radius])` | A filled rectangle. |
| `p:circle(x, y, r, fill, [stroke], [width])` | `fill` or `stroke` may be `nil`. |
| `p:text(x, y, text, [style])` | `style` as for `ui:label` plus `align`: two letters, horizontal `l`/`c`/`r` then vertical `t`/`c`/`b`, default `"lt"`. |
| `p:strip(top, bottom, top_colors, [bottom_colors])` | A band between two flat point lists with one colour per point: area fills and gradients. |

Since `width` sits before `color`, pass it, or `nil`, when you give a colour: `p:vline(x, 0, h, 1, "border")`.

A colour is a theme name (`bg`, `card`, `border`, `field`, `text`, `muted`, `subtle`, `holding`, `waiting`, `paused`, `failed`, `transparent`), `"#rrggbb"`, `"#rrggbbaa"`, or a number `0xrrggbb`.

The `cortisoff` global:

| Name | Half | Notes |
| --- | --- | --- |
| `cortisoff.platform` | both | `"windows"`, `"linux"` or `"macos"`. |
| `cortisoff.version` | both | The cortisoff version, as a string. |
| `cortisoff.json.encode(value)` | both | A Lua value to a JSON string. |
| `cortisoff.json.decode(text)` | both | A JSON string to a Lua value. `null` becomes `nil`. |
| `cortisoff.kelvin(k)` | window | The colour of a temperature, as `"#rrggbbaa"`. |
| `cortisoff.alpha(color, a)` | window | `color` with opacity `a` from 0 to 1, as `"#rrggbbaa"`. |
| `cortisoff.now()` | window | The local minute of the day, with its fraction. |

If `page` raises an error, what it drew is replaced by a red card with the message, and the rest of the window keeps working. A plugin without `page` shows "has nothing to set".

## Sandbox

Both halves run in the same sandbox:

- Available: the Lua base functions (`pairs`, `ipairs`, `pcall`, `error`, `type`, `tostring`, `tonumber`, `setmetatable` and the like), `table`, `string`, `math`, `bit`, and `cortisoff`.
- `require("lib.util")` loads `lib/util.lua` from the plugin's own folder, once. Nothing outside the folder can be named.
- `print` writes to the cortisoff log.
- Not available: `io`, `os`, `debug`, `package`, `load`, `loadstring`, `loadfile`, `dofile`, `module`, `string.dump`, `jit`. There is no file access, no process access and no way to load bytecode. Use `cx:now` or `cortisoff.now` for the time, `cx:run` to start a program, `cx:http` for the network.
- The JIT compiler is off: plugin code is interpreted, which is what lets cortisoff stop a call stuck in a loop after two seconds.

Everything a plugin does to the machine goes through `cx` and `ui`, which is how cortisoff can show what a plugin is allowed to do.

A plugin that sets `ffi = true` in its manifest gets LuaJIT's `ffi` global and with it native calls into any library: it can do anything the user's account can. The window marks such a plugin with that warning, and its page does not run until the user switches it on. Ask for FFI only when there is no other way.

## Installing and sharing

Plugins come from three places, read in this order. A later one with the same id replaces an earlier one, so a linked working copy of a built-in plugin is what runs.

1. Built in: the four in `plugins/`, compiled into cortisoff. They can be switched off, not removed.
2. Installed: one folder per plugin, named after its id, under
   - Windows: `%LOCALAPPDATA%\cortisoff\plugins`
   - Linux: `$XDG_DATA_HOME/cortisoff/plugins`, default `~/.local/share/cortisoff/plugins`
   - macOS: `~/Library/Application Support/cortisoff/plugins`
3. Linked: folders added with `cortisoff-cli link <folder>` or the Add a plugin page. They run in place and reload on save.

Which plugins are on and their settings live in `plugins.json` next to that (`%APPDATA%\cortisoff` on Windows, `$XDG_CONFIG_HOME/cortisoff` on Linux, `~/Library/Application Support/cortisoff` on macOS). Only the daemon writes it.

To install, paste one of these into the window's Add a plugin page:

- A GitHub repository: `owner/repo`, `github.com/owner/repo` or the full URL. cortisoff downloads the asset named `plugin.zip` on the latest release; without one, that release's source archive; without a release, the default branch.
- A direct link to a zip, ending in `.zip`.
- A folder path on this computer, which is linked instead of installed.

The archive may be at most 32 MB and unpack to at most 128 MB. cortisoff uses the shallowest `plugin.toml` it finds, up to three folders deep, so a repository can keep the plugin in a subfolder. The manifest must parse and its `entry` file must exist. The plugin is installed under its id, replacing an older copy, and starts switched off. cortisoff stores the source link beside it, in `.cortisoff-source.json`, which the window's Update button uses. There is no install command in the CLI yet.

To share a plugin, put the folder in a public GitHub repository, or attach a `plugin.zip` to a release so users get exactly the files you chose. `cortisoff-cli remove <id>` deletes an installed plugin or unlinks a folder.
