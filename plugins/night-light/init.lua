-- Night light: the colour temperature over the day.
--
-- The daemon half claims display.color_temp and moves it a little every
-- half minute while a fade runs, each step fading into the next, so the
-- evening comes in without a visible jump. Between fades nothing changes,
-- so it sleeps until the next one starts. The window half draws the day and
-- edits the settings; both read the same table.

local plugin = {}

plugin.settings = {
  evening = 20 * 60,
  morning = 7 * 60,
  fade = 30,
  night = 3200,
  day = 6500,
  pause_when_filled = true,
  except = { "vlc.exe", "photoshop.exe" },
}

local CAP = "display.color_temp"
local STEP_MS = 30000

local function clock(minute)
  minute = math.floor(minute + 0.5) % 1440
  return string.format("%02d:%02d", math.floor(minute / 60), minute % 60)
end

local function round(x)
  return math.floor(x + 0.5)
end

-- Kelvin at a minute of the day: night inside the window, day outside, a
-- cosine fade inside each edge.
local function kelvin_at(s, minute)
  local into = (minute - s.evening) % 1440
  local window = (s.morning - s.evening) % 1440
  local fade = math.max(s.fade, 1)
  local strength = 0
  if into < window then
    local t = math.min(into / fade, (window - into) / fade, 1)
    strength = 0.5 - 0.5 * math.cos(math.pi * t)
  end
  return s.day + (s.night - s.day) * strength
end

-- Minutes from `minute` until the kelvin next starts to move, or nil while a
-- fade is running. Outside the night window that is the evening; on the
-- plateau, the start of the morning fade.
local function still_for(s, minute)
  local into = (minute - s.evening) % 1440
  local window = (s.morning - s.evening) % 1440
  local fade = math.max(s.fade, 1)
  if into >= window then
    return 1440 - into
  end
  if into < fade or window - into < fade then
    return nil
  end
  return window - fade - into
end

local function excepted(s, app)
  if not app then return false end
  local exe = string.lower(app.exe)
  for _, name in ipairs(s.except) do
    if string.lower(name) == exe then return true end
  end
  return false
end

-- ---- daemon ---------------------------------------------------------------

local paused_until, resume_timer, step_timer, stepping

local settle

-- The next look: a step while a fade runs, the start of the next fade
-- otherwise. A resume, a clock change or a new setting reaches `settle`
-- through an event and moves this along with it. A step already on its way
-- keeps its time: re-armed on every focus change, it slipped back each time
-- and the fade stood still while the user switched windows.
local function schedule(cx, s, minute)
  local still = minute and still_for(s, minute)
  if step_timer and stepping and not still then return end
  if step_timer then cx:cancel(step_timer) end
  stepping = not still
  -- Just past the edge, so the look lands inside the fade.
  local wait = still and still * 60000 + 50 or STEP_MS
  step_timer = cx:after(wait, function(cx, s)
    step_timer = nil
    settle(cx, s)
  end)
end

function settle(cx, s)
  local minute, mono = cx:now()
  local world = cx:world()
  local k = minute and kelvin_at(s, minute) or s.day
  local reason
  if paused_until and mono < paused_until then
    reason = "paused"
  elseif s.pause_when_filled and world.maximized then
    reason = "filled"
  elseif excepted(s, world.app) then
    reason = "except"
  elseif not minute then
    reason = "no clock"
  end

  if reason or k >= s.day - 1 then
    cx:release(CAP, { fade = reason and 1500 or STEP_MS })
  else
    local ok, err = cx:claim(CAP, round(k), { fade = STEP_MS })
    if not ok then reason = err end
  end
  cx:publish({
    kelvin = round(k),
    holding = reason == nil and k < s.day - 1,
    reason = reason,
    paused_until = paused_until and minute and clock(minute + (paused_until - mono) / 60000),
  })
  schedule(cx, s, minute)
end

function plugin.start(cx, s)
  step_timer = nil
  cx:watch("app.focus")
  cx:watch("app.maximized")
  cx:watch("clock")
  settle(cx, s)
end

function plugin.on(cx, s, ev)
  if ev.kind == "message" and ev.name == "pause" then
    local _, mono = cx:now()
    paused_until = mono + (ev.data and ev.data.minutes or 60) * 60000
    if resume_timer then cx:cancel(resume_timer) end
    resume_timer = cx:after(paused_until - mono + 50, function(cx, s)
      paused_until, resume_timer = nil, nil
      settle(cx, s)
    end)
  elseif ev.kind == "message" and ev.name == "resume" then
    paused_until = nil
    if resume_timer then cx:cancel(resume_timer) end
    resume_timer = nil
  end
  settle(cx, s)
end

function plugin.stop(cx)
  cx:release(CAP, { fade = 1500 })
end

-- ---- window ---------------------------------------------------------------

local function timeline(ui, s, now)
  local kelvin, alpha = cortisoff.kelvin, cortisoff.alpha
  ui:canvas(nil, 150, function(p)
    local w, h = p:size()
    local top, bottom = 6, h - 24
    local lo, hi = 2400, 6800
    local function x_of(m) return w * m / 1440 end
    local function y_of(k) return bottom - (k - lo) / (hi - lo) * (bottom - top) end

    for hour = 0, 24, 3 do
      local x = x_of(hour * 60)
      p:vline(x, top, bottom, 1, "border")
      local align = (hour == 0 and "lt") or (hour == 24 and "rt") or "ct"
      p:text(x, bottom + 8, string.format("%02d:00", hour), { size = 12, color = "subtle", align = align })
    end

    local curve, floor, fill, fade = {}, {}, {}, {}
    for m = 0, 1440, 4 do
      local k = kelvin_at(s, m)
      local x = x_of(m)
      curve[#curve + 1], curve[#curve + 2] = x, y_of(k)
      floor[#floor + 1], floor[#floor + 2] = x, bottom
      fill[#fill + 1] = alpha(kelvin(k), 0.22)
      fade[#fade + 1] = alpha(kelvin(k), 0.04)
    end
    p:strip(curve, floor, fill, fade)

    local upper, lower, colours = {}, {}, {}
    for i = 0, 240 do
      local x = 3 + (w - 6) * i / 240
      upper[#upper + 1], upper[#upper + 2] = x, bottom - 6
      lower[#lower + 1], lower[#lower + 2] = x, bottom
      colours[#colours + 1] = kelvin(kelvin_at(s, i * 6))
    end
    p:strip(upper, lower, colours)
    p:circle(3, bottom - 3, 3, colours[1])
    p:circle(w - 3, bottom - 3, 3, colours[#colours])
    p:line(curve, 2, "text")

    for _, mark in ipairs({ { s.day, "day" }, { s.night, "night" } }) do
      p:text(6, y_of(mark[1]) - 5, string.format("%s  %d K", mark[2], round(mark[1])),
        { size = 12, color = "muted", align = "lb" })
    end

    local x, k = x_of(now), kelvin_at(s, now)
    p:vline(x, top, bottom, 1, "muted")
    p:circle(x, y_of(k), 6, kelvin(k), "text", 2)
    p:rect(x - 22, top, 44, 20, "text", 10)
    p:text(x, top + 10, "now", { size = 12, strong = true, color = "bg", align = "cc" })
  end)
end

local REASONS = {
  paused = "Paused",
  filled = "An app fills the screen",
  except = "Not in this app",
  ["no clock"] = "No clock",
}

function plugin.status(s, st)
  st = st or {}
  if st.reason == "paused" then
    return { line = "Paused", detail = "Back at " .. (st.paused_until or "?"), state = "paused" }
  elseif st.reason and REASONS[st.reason] then
    return { line = REASONS[st.reason], detail = "The screen keeps its own colours for now", state = "paused" }
  elseif st.reason then
    return { line = "Cannot tint", detail = st.reason, state = "failed" }
  elseif st.holding then
    return {
      line = string.format("%d K", st.kelvin),
      detail = string.format("Warm until %s", clock(s.morning)),
      state = "holding",
    }
  end
  return { line = "Daylight", detail = "Warms at " .. clock(s.evening), state = "waiting" }
end

local new_app = ""

function plugin.page(ui, s, st)
  st = st or {}
  local now = cortisoff.now()

  ui:card(nil, function(ui)
    ui:horizontal(function(ui)
      local k = st.kelvin or kelvin_at(s, now)
      ui:canvas(40, 40, function(p) p:circle(20, 20, 18, cortisoff.kelvin(k)) end)
      ui:space(6)
      ui:vertical(function(ui)
        ui:label(string.format("%d K", round(k)), { size = 26, strong = true })
        ui:hint(plugin.status(s, st).detail)
      end, 0)
      ui:right(function(ui)
        if st.reason == "paused" then
          if ui:button("Resume", "arrow") then ui:send("resume") end
        else
          if ui:button("Pause for 1 hour", "pause") then ui:send("pause", { minutes = 60 }) end
        end
      end)
    end)
    ui:space(10)
    timeline(ui, s, now)
  end)
  ui:space(12)

  ui:card("Schedule", function(ui)
    ui:row("Evening", "When the screen starts to warm.", function(ui) s.evening = ui:time(s.evening) end)
    ui:divider()
    ui:row("Morning", "When daylight is back.", function(ui) s.morning = ui:time(s.morning) end)
    ui:divider()
    ui:row("Night temperature", nil, function(ui)
      s.night = ui:slider(s.night, {
        min = 1900, max = 6500, step = 100, track = "kelvin", preview = "display.color_temp",
        text = function(v) return string.format("%d K", round(v)) end,
      })
    end)
    ui:divider()
    ui:row("Fade", "How long each change takes.", function(ui)
      s.fade = ui:slider(s.fade, {
        min = 0, max = 120, step = 1,
        text = function(v) return string.format("%d min", round(v)) end,
      })
    end)
  end)
  ui:space(12)

  ui:card("Exceptions", function(ui)
    ui:row("Pause while an app fills the screen", "Games, films and maximised windows keep their colours.",
      function(ui) s.pause_when_filled = ui:switch(s.pause_when_filled) end)
    ui:divider()
    ui:row("Never in these apps", nil, function(ui)
      new_app = ui:text(new_app, { hint = "app.exe", width = 140 })
      if ui:button("Add", "add") and new_app ~= "" then
        table.insert(s.except, string.lower(new_app))
        new_app = ""
      end
    end)
    ui:wrap(function(ui)
      for i, app in ipairs(s.except) do
        if ui:chip(app, "close") then
          table.remove(s.except, i)
          break
        end
      end
    end)
  end)
end

return plugin
