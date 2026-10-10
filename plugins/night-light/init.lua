-- Night light: the colour temperature over the day.

local kelvin, alpha = cortisoff.kelvin, cortisoff.alpha

local plugin = {}

plugin.settings = {
  enabled = true,
  evening = 20 * 60,
  morning = 7 * 60,
  fade = 30,
  night = 3200,
  day = 6600,
  pause_when_filled = true,
  except = { "vlc.exe", "photoshop.exe" },
  screens = {
    { name = "Display 1", detail = "2560 x 1440, SDR, gamma ramp", on = true },
    { name = "Display 2", detail = "1920 x 1080, SDR, gamma ramp", on = true },
    { name = "Display 3", detail = "2560 x 1440, HDR, panel gain", on = false },
  },
}

local function clock(minute)
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

local function timeline(ui, s, now)
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

    -- The area under the curve, in the colour the screen has at each
    -- minute, faint so the line leads.
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

    -- What the screen looks like, minute by minute, as a ribbon.
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

function plugin.status(s)
  local on = 0
  for _, screen in ipairs(s.screens) do
    if screen.on then on = on + 1 end
  end
  return {
    line = string.format("%d K until %s", round(s.night), clock(s.morning)),
    detail = string.format("Holding %d K on %d of %d screens since %s",
      round(s.night), on, #s.screens, clock(s.evening)),
    state = s.enabled and "holding" or "waiting",
  }
end

function plugin.page(ui, s)
  local now = cortisoff.now()

  ui:header("moon", "Night light", "Warms the screen in the evening and gives it back in the morning.",
    function(ui)
      s.enabled = ui:switch(s.enabled)
      ui:space(6)
      ui:button("Pause for 1 hour", "pause")
    end)

  ui:card(nil, function(ui)
    ui:horizontal(function(ui)
      local k = kelvin_at(s, now)
      ui:canvas(40, 40, function(p) p:circle(20, 20, 18, kelvin(k)) end)
      ui:space(6)
      ui:vertical(function(ui)
        ui:label(string.format("%d K", round(k)), { size = 26, strong = true })
        ui:hint(string.format("Holding since %s. Daylight comes back at %s.", clock(s.evening), clock(s.morning)))
      end, 0)
    end)
    ui:space(10)
    timeline(ui, s, now)
  end)
  ui:space(12)

  ui:card("Schedule", function(ui)
    ui:row("Evening", "When the screen starts to warm.", function(ui) ui:field(clock(s.evening)) end)
    ui:divider()
    ui:row("Morning", "When daylight is back.", function(ui) ui:field(clock(s.morning)) end)
    ui:divider()
    ui:row("Night temperature", nil, function(ui)
      s.night = ui:slider(s.night, {
        min = 1900, max = 6500, step = 100, track = "kelvin",
        text = function(v) return string.format("%d K", round(v)) end,
      })
    end)
    ui:divider()
    ui:row("Fade", "How long each change takes.", function(ui)
      s.fade = ui:slider(s.fade, {
        min = 0, max = 60, step = 1,
        text = function(v) return string.format("%d min", round(v)) end,
      })
    end)
  end)
  ui:space(12)

  ui:card("Exceptions", function(ui)
    ui:row("Pause while an app fills the screen", "Games, films and maximised windows keep their colours.",
      function(ui) s.pause_when_filled = ui:switch(s.pause_when_filled) end)
    ui:divider()
    ui:row("Never in these apps", nil, function(ui) ui:button("Add an app", "add") end)
    ui:wrap(function(ui)
      for _, app in ipairs(s.except) do ui:chip(app, "close") end
    end)
  end)
  ui:space(12)

  ui:card("Screens", function(ui)
    for i, screen in ipairs(s.screens) do
      ui:row(screen.name, screen.detail, function(ui) screen.on = ui:switch(screen.on) end)
      if i < #s.screens then ui:divider() end
    end
  end)
end

return plugin
