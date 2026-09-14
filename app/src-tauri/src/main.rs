// The dashboard opens no console window of its own on Windows.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! The cortisoff dashboard.
//!
//! An IPC client with a window. It links the protocol and the platform's
//! transport, and it links nothing else: no actuator, no rule engine, no
//! knowledge of what a rule means. Everything on screen came out of the daemon
//! over the pipe, which is the same contract `cortisoff` the CLI has.
//!
//! That is why this exists as a separate executable. The daemon must not link
//! a webview, and the dashboard must not be able to change a screen behind the
//! daemon's back.

mod transport;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cortisoff_host::{CapabilityId, Value};
use cortisoff_ipc::{DoctorReport, Notification, Request, Response, Rule, StateReport};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Listener};

/// Emitted whenever the connection to the daemon comes up or goes down, so the
/// window can say "not running" instead of showing a stale world forever.
#[derive(Debug, Clone, Serialize)]
struct Status {
    connected: bool,
    /// A sentence for a person, `None` while connected.
    reason: Option<String>,
}

const STATUS_EVENT: &str = "daemon://status";
const NOTIFICATION_EVENT: &str = "daemon://notification";

fn main() {
    // Before anything else starts: two windows on one daemon would both be
    // right, and the second one is never what the person double-clicking
    // wanted. The tray does this too, it launches a fresh process every time.
    // `tauri-plugin-single-instance` was here for this and fell through on the
    // installed build, mutex held and hidden window never found, so the claim
    // is a plain named mutex taken before Tauri exists. Other platforms are
    // not started and get nothing here yet.
    #[cfg(windows)]
    let _instance = match cortisoff_win::claim_dashboard() {
        Some(lock) => lock,
        None => {
            cortisoff_win::raise_dashboard();
            return;
        }
    };

    tauri::Builder::default()
        // Everything but VISIBLE: the window is built hidden on purpose and
        // shown once, and a state file written while it was up would defeat
        // that and flash an unpainted webview at every launch.
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(
                    tauri_plugin_window_state::StateFlags::all()
                        & !tauri_plugin_window_state::StateFlags::VISIBLE,
                )
                .build(),
        )
        // The updater downloads and verifies a signed payload, so it is the
        // one plugin that turns a bad manifest into code execution. `process`
        // is here for the relaunch that follows an install.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .invoke_handler(tauri::generate_handler![
            doctor,
            state,
            rules,
            save_rules,
            set,
            apply,
            read_value,
            clear,
            preview,
            preview_end,
            usage,
            reload,
            stop_daemon,
            autostart,
            set_autostart
        ])
        .manage(Samples::default())
        .manage(PreviewLink::default())
        .setup(|app| {
            let window = tauri::WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::App("index.html".into()),
            )
            .title("cortisoff")
            .inner_size(940.0, 680.0)
            .min_inner_size(720.0, 520.0)
            // The window is translucent and the OS blurs what is behind it, so
            // it has to be built transparent and painted by the page.
            .transparent(true)
            .background_color(tauri::webview::Color(0, 0, 0, 0))
            .decorations(false)
            .visible(false)
            .center()
            .build()?;

            watch(app.handle().clone());
            window.show()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("the dashboard could not start");
}

/// One thread holding one subscribed connection, reconnecting on its own.
///
/// The daemon is allowed not to be running, and starting the dashboard first
/// has to be a normal thing to do rather than an error dialog. So this never
/// gives up: it reports the state and tries again.
fn watch(app: AppHandle) {
    let alive = Arc::new(AtomicBool::new(true));
    let stopping = alive.clone();
    // The window closing is the whole application closing, and a blocking read
    // on a dead pipe returns on its own, so this only has to stop the retries.
    app.listen("tauri://destroyed", move |_| {
        stopping.store(false, Ordering::Relaxed);
    });

    std::thread::spawn(move || {
        let mut announced_down = false;
        // Once per launch. Someone who stopped the daemon from this window
        // and is still looking at it does not want it back 800 ms later.
        let mut tried_starting = false;
        while alive.load(Ordering::Relaxed) {
            match transport::subscribe() {
                Ok(mut client) => {
                    announced_down = false;
                    let _ = app.emit(
                        STATUS_EVENT,
                        Status {
                            connected: true,
                            reason: None,
                        },
                    );
                    // Returns when the daemon hangs up, which `stop` does on
                    // purpose and a crash does by accident. Both are the same
                    // thing here: reconnect.
                    while let Some(frames) = client.recv() {
                        for frame in frames {
                            if let cortisoff_ipc::Frame::Notification { notification } = frame {
                                let _ = app.emit(NOTIFICATION_EVENT, &notification);
                                if matches!(notification, Notification::Closing) {
                                    break;
                                }
                            }
                        }
                    }
                }
                Err(reason) => {
                    // An installed cortisoff opened before its daemon, which
                    // is the first launch after installing and any launch
                    // after quitting from the tray.
                    if !tried_starting {
                        tried_starting = true;
                        if transport::start_daemon().is_ok() {
                            std::thread::sleep(std::time::Duration::from_millis(400));
                            continue;
                        }
                    }
                    if !announced_down {
                        announced_down = true;
                        let _ = app.emit(
                            STATUS_EVENT,
                            Status {
                                connected: false,
                                reason: Some(reason),
                            },
                        );
                    }
                }
            }
            if alive.load(Ordering::Relaxed) {
                let _ = app.emit(
                    STATUS_EVENT,
                    Status {
                        connected: false,
                        reason: Some("the daemon is not running".to_owned()),
                    },
                );
                std::thread::sleep(std::time::Duration::from_millis(800));
            }
        }
    });
}

/// Every command below is `async` for one reason: a Tauri command that is
/// not runs on the main thread, and every one of these opens a pipe and
/// blocks on the daemon's answer. The window went stiff for the length of a
/// call, and three questions asked together were answered one after the
/// other. On the runtime's blocking pool they overlap and the window keeps
/// painting.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|e| format!("the call was dropped before it answered: {e}"))?
}

/// The daemon answering "no" is not the dashboard failing. Its sentence was
/// written to be shown to a person, so it is shown to a person unchanged.
fn plain(response: Response) -> Result<serde_json::Value, String> {
    match response {
        Response::Error { message } => Err(message),
        Response::Ok => Ok(serde_json::Value::Null),
        Response::Doctor(report) => serde_json::to_value(*report).map_err(|e| e.to_string()),
        Response::State(report) => serde_json::to_value(*report).map_err(|e| e.to_string()),
        Response::Rules { rules } => serde_json::to_value(rules).map_err(|e| e.to_string()),
    }
}

#[tauri::command]
async fn doctor() -> Result<DoctorReport, String> {
    blocking(|| match transport::ask(Request::Doctor)? {
        Response::Doctor(report) => Ok(*report),
        Response::Error { message } => Err(message),
        other => Err(format!("the daemon answered a {other:?} to a doctor")),
    })
    .await
}

#[tauri::command]
async fn state() -> Result<StateReport, String> {
    blocking(|| match transport::ask(Request::State)? {
        Response::State(report) => Ok(*report),
        Response::Error { message } => Err(message),
        other => Err(format!("the daemon answered a {other:?} to a state")),
    })
    .await
}

#[tauri::command]
async fn rules() -> Result<Vec<Rule>, String> {
    blocking(|| match transport::ask(Request::Rules)? {
        Response::Rules { rules } => Ok(rules),
        Response::Error { message } => Err(message),
        other => Err(format!("the daemon answered a {other:?} to a rules")),
    })
    .await
}

/// The whole set, which is what makes a switch, an edit, an addition and a
/// deletion the same call. A set that does not load leaves the file alone and
/// comes back as the daemon's own sentence.
#[tauri::command]
async fn save_rules(rules: Vec<Rule>) -> Result<serde_json::Value, String> {
    blocking(move || plain(transport::ask(Request::SaveRules { rules })?)).await
}

/// A manual override, outranking every rule until it is cleared. The shape of
/// the value comes from `cortisoff_ipc::parse_value`, shared with the CLI so
/// the two cannot disagree about what `@240` means.
#[tauri::command]
async fn set(cap: String, value: String) -> Result<serde_json::Value, String> {
    blocking(move || {
        plain(transport::ask(Request::Set {
            cap: CapabilityId::new(&cap),
            value: cortisoff_ipc::parse_value(&value),
        })?)
    })
    .await
}

/// What `3400`, `@240` and `off` mean, answered by the same function the CLI
/// uses. The settings dialog edits values as the rule file spells them, and
/// reimplementing that in TypeScript is how the two would drift. Pure and
/// instant, so the one command that stays synchronous.
#[tauri::command]
fn read_value(text: String) -> Value {
    cortisoff_ipc::parse_value(&text)
}

/// The same override from a value that is already structured, which is what a
/// card holds: it knows what its rule sets and has no reason to render that to
/// text so the daemon can read it back.
#[tauri::command]
async fn apply(cap: String, value: Value) -> Result<serde_json::Value, String> {
    blocking(move || {
        plain(transport::ask(Request::Set {
            cap: CapabilityId::new(&cap),
            value,
        })?)
    })
    .await
}

#[tauri::command]
async fn clear(cap: String) -> Result<serde_json::Value, String> {
    blocking(move || {
        plain(transport::ask(Request::Clear {
            cap: CapabilityId::new(&cap),
        })?)
    })
    .await
}

/// The one connection a drag runs on, opened by the first `preview` and
/// dropped by `preview_end`. The daemon ties a preview to the connection that
/// sent it and ends it when that connection closes: through the throwaway
/// connection `ask` opens, every value was applied and then, on the close a
/// few milliseconds later, faded back, and the screen strobed under the
/// slider. Holding one client for the whole drag is the fix, and it keeps
/// the safety: if this window dies mid-drag the pipe closes with it and the
/// daemon fades back on its own.
#[derive(Default)]
struct PreviewLink(Arc<Mutex<Option<transport::Client>>>);

/// A slider mid-drag. Sent on every input event, so it must never block the
/// window, and it must be ended with `preview_end`.
#[tauri::command]
async fn preview(
    cap: String,
    value: Value,
    link: tauri::State<'_, PreviewLink>,
) -> Result<serde_json::Value, String> {
    let link = Arc::clone(&link.0);
    blocking(move || {
        let mut slot = link.lock().map_err(|e| e.to_string())?;
        let client = match slot.as_mut() {
            Some(client) => client,
            None => slot.insert(transport::connect()?),
        };
        let request = Request::Preview {
            cap: CapabilityId::new(&cap),
            value,
        };
        match client.ask(request) {
            Ok(response) => plain(response),
            Err(reason) => {
                // A dead connection is not one to keep asking: the next
                // preview opens a fresh one.
                *slot = None;
                Err(reason)
            }
        }
    })
    .await
}

/// The drag ended. Answered on the drag's own connection, which is then
/// closed, so the daemon's fade back starts from the last previewed value.
#[tauri::command]
async fn preview_end(
    cap: String,
    link: tauri::State<'_, PreviewLink>,
) -> Result<serde_json::Value, String> {
    let link = Arc::clone(&link.0);
    blocking(move || {
        let mut slot = link.lock().map_err(|e| e.to_string())?;
        let Some(mut client) = slot.take() else {
            // Nothing was previewing on this window, which is what a release
            // with no drag before it looks like. Not an error.
            return Ok(serde_json::Value::Null);
        };
        plain(client.ask(Request::PreviewEnd {
            cap: CapabilityId::new(&cap),
        })?)
    })
    .await
}

/// The last CPU reading per pid, so the next call can turn a total into a
/// rate. Kept in Tauri's state rather than in the page so a page reload does
/// not show a bogus first sample.
#[derive(Default)]
struct Samples(Mutex<HashMap<u32, (Instant, u64)>>);

/// What one process costs right now, as the task manager would show it.
#[derive(Debug, Clone, Serialize)]
struct UsageReport {
    pid: u32,
    /// Percent of the whole machine, every core counted. `None` on the first
    /// reading, when there is no interval to divide by yet.
    cpu: Option<f64>,
    rss_bytes: u64,
}

/// CPU and memory of the given processes: the daemon's pid comes from
/// `doctor`, and pid 0 stands for this window. A pid that is gone is left
/// out rather than reported as zero.
#[tauri::command]
async fn usage(
    pids: Vec<u32>,
    samples: tauri::State<'_, Samples>,
) -> Result<Vec<UsageReport>, String> {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1) as f64;
    let mut out = Vec::new();
    let mut last = samples.0.lock().map_err(|e| e.to_string())?;
    let now = Instant::now();
    for pid in pids {
        let real = if pid == 0 { std::process::id() } else { pid };
        let Some((cpu_ms, rss_bytes)) = transport::usage(real) else {
            last.remove(&pid);
            continue;
        };
        let cpu = last.insert(pid, (now, cpu_ms)).and_then(|(then, before)| {
            let wall = now.duration_since(then).as_secs_f64() * 1000.0;
            (wall > 0.0).then(|| (cpu_ms.saturating_sub(before) as f64 / wall) * 100.0 / cores)
        });
        out.push(UsageReport {
            pid,
            cpu,
            rss_bytes,
        });
    }
    Ok(out)
}

#[tauri::command]
async fn reload() -> Result<serde_json::Value, String> {
    blocking(|| plain(transport::ask(Request::Reload)?)).await
}

/// Named `stop_daemon` rather than `stop` because it stops the daemon and not
/// this window, and someone reading the frontend should not have to guess.
#[tauri::command]
async fn stop_daemon() -> Result<serde_json::Value, String> {
    blocking(|| plain(transport::ask(Request::Shutdown)?)).await
}

/// Whether the daemon starts when the user logs on. Answered from Task
/// Scheduler, not from the daemon, so it works while the daemon is down. A
/// COM call into the scheduler service, so off the main thread like the rest.
#[tauri::command]
async fn autostart() -> bool {
    blocking(|| Ok(transport::autostart()))
        .await
        .unwrap_or(false)
}

#[tauri::command]
async fn set_autostart(on: bool) -> Result<(), String> {
    blocking(move || transport::set_autostart(on)).await
}
