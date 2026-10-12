-- Wallpaper Engine pause: stops the wallpaper while nobody is at the machine,
-- and starts it again when someone is back.
--
-- Wallpaper Engine already pauses by itself for an app in front, maximised or
-- fullscreen, for another app's sound, on battery and per application, in
-- its own settings. This only adds what it cannot see: a locked session and
-- an empty chair.

local plugin = {}

plugin.settings = {
  when_locked = true,
  when_idle = true,
  idle_minutes = 5,
}

local CAP = "wallpaper.playback"

local state = { locked = false, idle = false }

local LABELS = {
  locked = "the session is locked",
  idle = "nobody is at the machine",
}

-- ---- daemon ---------------------------------------------------------------

local function why(s)
  if s.when_locked and state.locked then return "locked" end
  if s.when_idle and state.idle then return "idle" end
end

local function settle(cx, s)
  local reason = why(s)
  local problem
  if reason then
    local ok, err = cx:claim(CAP, false)
    if not ok then problem = err end
  else
    cx:release(CAP)
  end
  cx:publish({ paused = reason ~= nil and problem == nil, reason = reason and LABELS[reason], problem = problem })
end

local function rewatch(cx, s)
  cx:unwatch("session.lock")
  cx:unwatch("user.idle")
  if s.when_locked then cx:watch("session.lock") end
  if s.when_idle then cx:watch("user.idle", { after = math.max(1, s.idle_minutes) * 60 }) end
end

function plugin.start(cx, s)
  local w = cx:world()
  state.locked, state.idle = w.locked, w.idle
  rewatch(cx, s)
  settle(cx, s)
end

function plugin.on(cx, s, ev)
  if ev.kind == "session.lock" then state.locked = ev.locked
  elseif ev.kind == "user.idle" then state.idle = ev.idle
  elseif ev.kind == "settings" then rewatch(cx, s)
  end
  settle(cx, s)
end

function plugin.stop(cx)
  cx:release(CAP)
end

-- ---- window ---------------------------------------------------------------

function plugin.status(s, st)
  st = st or {}
  if st.problem then
    return { line = "Cannot pause", detail = st.problem, state = "failed" }
  elseif st.paused then
    return { line = "Paused", detail = "Because " .. (st.reason or "?"), state = "paused" }
  end
  return { line = "Playing", detail = "Nothing asks it to stop", state = "waiting" }
end

function plugin.page(ui, s, st)
  ui:card("Pause Wallpaper Engine when", function(ui)
    ui:row("The session is locked", nil, function(ui) s.when_locked = ui:switch(s.when_locked) end)
    ui:divider()
    ui:row("Nobody touches the machine", nil, function(ui) s.when_idle = ui:switch(s.when_idle) end)
    if s.when_idle then
      ui:row("For", nil, function(ui)
        s.idle_minutes = ui:slider(s.idle_minutes, {
          min = 1, max = 60, step = 1,
          text = function(v) return string.format("%d min", v) end,
        })
      end)
    end
  end)
  ui:space(8)
  ui:hint("Apps in front, fullscreen, sound and battery are in Wallpaper Engine's own settings, under Performance.")
end

return plugin
