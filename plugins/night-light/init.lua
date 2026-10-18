-- Night light: the colour temperature over the day.
--
-- The daemon half asks for the night colour once at the evening edge and lets
-- it go once at the morning edge. cortisoff runs the fade itself, frame by
-- frame, so nothing here steps it; between the edges the plugin sleeps. The
-- window half draws the day and edits the settings; both read the same table.

local plugin = {}

plugin.settings = {
  evening = 20 * 60,
  morning = 7 * 60,
  fade_seconds = 60,
  night = 3200,
  day = 6500,
  pause_when_filled = true,
  except = { "vlc.exe", "photoshop.exe" },
}

local CAP = "display.color_temp"
local MAX_FADE = 300
-- A fullscreen game starting, an exception or a pause: the colour gets out of
-- the way quickly whatever the evening fade is, and comes back as quickly.
-- Always a fade, even with the evening one set to instant: a screen that
-- snaps from 3200 K to daylight as a game opens reads as a glitch.
local QUICK_MS = 800

local function clock(minute)
  minute = math.floor(minute + 0.5) % 1440
  return string.format("%02d:%02d", math.floor(minute / 60), minute % 60)
end

local function round(x)
  return math.floor(x + 0.5)
end

-- Seconds each change takes, 0 for at once. Up to 0.2 the setting was
-- `fade`, in minutes; a stored one is read once and replaced.
local function fade_s(s)
  if s.fade ~= nil then
    return math.min(math.max(s.fade, 0) * 60, MAX_FADE)
  end
  return math.min(math.max(s.fade_seconds or 0, 0), MAX_FADE)
end

-- Where `minute` sits: inside the night window or not, and how many minutes
-- since the edge that started the current half.
local function phase(s, minute)
  local into = (minute - s.evening) % 1440
  local window = (s.morning - s.evening) % 1440
  if into < window then
    return true, into
  end
  return false, into - window
end

-- Kelvin at a minute of the day, for the drawing: each edge fades for
-- `fade_s` seconds, then the colour holds.
local function kelvin_at(s, minute)
  local night, since = phase(s, minute)
  local fade = fade_s(s) / 60
  local from, to = s.night, s.day
  if night then from, to = s.day, s.night end
  if fade > 0 and since < fade then
    local t = since / fade
    t = t * t * (3 - 2 * t)
    return from + (to - from) * t
  end
  return to
end

-- Minutes until the next edge.
local function next_edge(s, minute)
  local into = (minute - s.evening) % 1440
  local window = (s.morning - s.evening) % 1440
  if into < window then
    return window - into
  end
  return 1440 - into
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

local paused_until, resume_timer, edge_timer
-- The next change is quick rather than a whole evening fade: the way back
-- from a reason to stand aside, or a setting just edited.
local quick_next = false
-- Outputs a fullscreen app covers, given their own colours while the others
-- stay warm. False once the platform said it cannot hold one output apart:
-- then a fullscreen app pauses every screen, as before.
local spared, apart = {}, true

local settle

-- Give the outputs in `keep` back to the night, quickly. Every one when
-- `keep` is empty.
local function unspare(cx, keep)
  for output in pairs(spared) do
    if not keep[output] then
      cx:release(CAP, { output = output, fade = QUICK_MS })
      spared[output] = nil
    end
  end
end

-- Hold each filled output at the colour it has with nothing on it. False when
-- that cannot be done here, after letting go of any already held.
local function spare(cx, outputs)
  local rest = cx:rest(CAP)
  for output in pairs(outputs) do
    if not spared[output] then
      local ok = rest and cx:claim(CAP, rest, { output = output, fade = QUICK_MS })
      if not ok then
        apart = false
        unspare(cx, {})
        return false
      end
      spared[output] = true
    end
  end
  return true
end

function settle(cx, s)
  local minute, mono = cx:now()
  local world = cx:world()
  local night = minute and phase(s, minute)
  local fade = fade_s(s) * 1000
  local filled = {}
  for _, output in ipairs(world.filled or {}) do filled[output] = true end
  local some_filled = next(filled) ~= nil
  local reason
  if paused_until and mono < paused_until then
    reason = "paused"
  elseif s.pause_when_filled and world.fullscreen and not (apart and some_filled) then
    -- Fullscreen with no output named, or outputs that cannot be held apart.
    reason = "filled"
  elseif excepted(s, world.app) then
    reason = "except"
  elseif not minute then
    reason = "no clock"
  end

  -- Outputs a game still fills stay spared while the whole lets go: released
  -- together, the game's output would rejoin the whole as it fades, and the
  -- morning would put the night back on the game for up to five minutes.
  local function still_filled()
    local out = {}
    if s.pause_when_filled and apart then
      for output in pairs(spared) do
        if filled[output] then out[output] = true end
      end
    end
    return out
  end

  local keep = {}
  if reason then
    cx:release(CAP, { fade = QUICK_MS })
    keep = still_filled()
  elseif night then
    local ok, err = cx:claim(CAP, round(s.night), { fade = quick_next and QUICK_MS or fade })
    if not ok then
      reason = err
    elseif s.pause_when_filled and some_filled and apart then
      if spare(cx, filled) then
        keep = filled
      elseif world.fullscreen then
        reason = "filled"
        cx:release(CAP, { fade = QUICK_MS })
      end
    end
  else
    cx:release(CAP, { fade = quick_next and QUICK_MS or fade })
    keep = still_filled()
    -- A game opened while the morning still fades: the others are warm yet.
    if not quick_next and s.pause_when_filled and apart and some_filled
      and kelvin_at(s, minute) < s.day and spare(cx, filled) then
      keep = filled
    end
  end
  unspare(cx, keep)
  quick_next = reason ~= nil

  local count = 0
  for _ in pairs(spared) do count = count + 1 end
  cx:publish({
    kelvin = round(minute and kelvin_at(s, minute) or s.day),
    holding = reason == nil and night == true,
    spared = count,
    reason = reason,
    paused_until = paused_until and minute and clock(minute + (paused_until - mono) / 60000),
  })

  if edge_timer then cx:cancel(edge_timer) end
  edge_timer = nil
  if minute then
    -- Just past the edge, so the look lands on the new side of it.
    edge_timer = cx:after(math.ceil(next_edge(s, minute) * 60000) + 50, function(cx, s)
      edge_timer = nil
      settle(cx, s)
    end)
  end
end

function plugin.start(cx, s)
  edge_timer, quick_next = nil, false
  spared, apart = {}, true
  cx:watch("app.focus")
  cx:watch("app.fullscreen")
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
  elseif ev.kind == "settings" then
    quick_next = true
  end
  settle(cx, s)
end

function plugin.stop(cx)
  unspare(cx, {})
  cx:release(CAP, { fade = QUICK_MS })
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

    -- The curve is sampled every 4 minutes, plus both edges and the end of
    -- each fade, so a fade of a few seconds still draws as a clean step.
    local marks = {}
    for m = 0, 1440, 4 do marks[#marks + 1] = m end
    local fade = fade_s(s) / 60
    for _, edge in ipairs({ s.evening, s.morning }) do
      marks[#marks + 1] = edge
      marks[#marks + 1] = edge + math.max(fade, 0.01)
      marks[#marks + 1] = edge - 0.01
    end
    local kept = {}
    for _, m in ipairs(marks) do
      if m >= 0 and m <= 1440 then kept[#kept + 1] = m end
    end
    table.sort(kept)

    local curve, floor, fill, faded = {}, {}, {}, {}
    for _, m in ipairs(kept) do
      local k = kelvin_at(s, m)
      local x = x_of(m)
      curve[#curve + 1], curve[#curve + 2] = x, y_of(k)
      floor[#floor + 1], floor[#floor + 2] = x, bottom
      fill[#fill + 1] = alpha(kelvin(k), 0.22)
      faded[#faded + 1] = alpha(kelvin(k), 0.04)
    end
    p:strip(curve, floor, fill, faded)

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
  filled = "An app is fullscreen",
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
    local detail = string.format("Warm until %s", clock(s.morning))
    if (st.spared or 0) > 0 then
      detail = detail .. ", except the fullscreen screen"
    end
    return { line = string.format("%d K", st.kelvin), detail = detail, state = "holding" }
  end
  return { line = "Daylight", detail = "Warms at " .. clock(s.evening), state = "waiting" }
end

local function duration(v)
  v = round(v)
  if v == 0 then return "Instant" end
  if v < 60 then return string.format("%d s", v) end
  local m, sec = math.floor(v / 60), v % 60
  if sec == 0 then return string.format("%d min", m) end
  return string.format("%d min %02d", m, sec)
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
    ui:row("Fade", "How long the evening and the morning take.", function(ui)
      local was = fade_s(s)
      local v = ui:slider(was, { min = 0, max = MAX_FADE, step = 5, text = duration })
      if v ~= was or s.fade ~= nil then
        s.fade_seconds, s.fade = v, nil
      end
    end)
  end)
  ui:space(12)

  ui:card("Exceptions", function(ui)
    ui:row("Pause while an app is fullscreen", "Games and films keep their colours on their screen.",
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
