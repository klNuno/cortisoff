-- Wallpaper pause: stops an animated wallpaper while nobody can see it or
-- while it costs too much, and starts it again after.

local plugin = {}

plugin.settings = {
  when_filled = true,
  on_battery = true,
  when_locked = true,
  when_idle = true,
  idle_minutes = 5,
  with_sound = false,
}

local CAP = "wallpaper.playback"

local state = { filled = false, battery = false, locked = false, idle = false, sound = false }

local LABELS = {
  filled = "an app fills the screen",
  battery = "on battery",
  locked = "the session is locked",
  idle = "nobody is at the machine",
  sound = "sound is playing",
}

-- ---- daemon ---------------------------------------------------------------

local function why(s)
  if s.when_locked and state.locked then return "locked" end
  if s.when_filled and state.filled then return "filled" end
  if s.on_battery and state.battery then return "battery" end
  if s.when_idle and state.idle then return "idle" end
  if s.with_sound and state.sound then return "sound" end
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
  for _, kind in ipairs({ "app.maximized", "power.source", "session.lock", "user.idle", "audio.active" }) do
    cx:unwatch(kind)
  end
  if s.when_filled then cx:watch("app.maximized") end
  if s.on_battery then cx:watch("power.source") end
  if s.when_locked then cx:watch("session.lock") end
  if s.when_idle then cx:watch("user.idle", { after = math.max(1, s.idle_minutes) * 60 }) end
  if s.with_sound then cx:watch("audio.active") end
end

function plugin.start(cx, s)
  local w = cx:world()
  state.filled, state.battery, state.locked = w.maximized, w.on_battery, w.locked
  state.idle, state.sound = w.idle, w.audio
  rewatch(cx, s)
  settle(cx, s)
end

function plugin.on(cx, s, ev)
  if ev.kind == "app.maximized" then state.filled = ev.maximized
  elseif ev.kind == "power.source" then state.battery = ev.on_battery
  elseif ev.kind == "session.lock" then state.locked = ev.locked
  elseif ev.kind == "user.idle" then state.idle = ev.idle
  elseif ev.kind == "audio.active" then state.sound = ev.active
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
  ui:card("Pause the wallpaper when", function(ui)
    ui:row("An app fills the screen", "Games, films, maximised windows.",
      function(ui) s.when_filled = ui:switch(s.when_filled) end)
    ui:divider()
    ui:row("Running on battery", nil, function(ui) s.on_battery = ui:switch(s.on_battery) end)
    ui:divider()
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
    ui:divider()
    ui:row("Sound is playing", "For wallpapers that play their own audio.",
      function(ui) s.with_sound = ui:switch(s.with_sound) end)
  end)
end

return plugin
