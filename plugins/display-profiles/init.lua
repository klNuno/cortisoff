-- Display profiles: a refresh rate and a vibrance per application, applied
-- while it has the focus and given back when it loses it.

local plugin = {}

plugin.settings = {
  profiles = {
    { exe = "cs2.exe", refresh = 0, vibrance = 80 },
  },
}

local MODE, VIBRANCE, FOCUSED = "display.mode", "display.vibrance", "app.focus"
-- Above other plugins: the app in front decides.
local FOCUS = 100

local function find(s, exe)
  if not exe then return nil end
  exe = string.lower(exe)
  for _, p in ipairs(s.profiles) do
    if string.lower(p.exe or "") == exe then return p end
  end
end

-- ---- daemon ---------------------------------------------------------------

local function apply(cx, s)
  local world = cx:world()
  local app = world.app and world.app.exe
  local p = find(s, app)
  local held, problems = {}, {}
  if p and (p.refresh or 0) > 0 and cx:has(MODE) then
    local ok, err = cx:claim(MODE, { refresh = p.refresh }, { priority = FOCUS })
    if ok then held[#held + 1] = string.format("%d Hz", p.refresh) else problems[#problems + 1] = err end
  else
    cx:release(MODE)
  end
  if p and p.vibrance and p.vibrance >= 0 and cx:has(VIBRANCE) then
    local ok, err = cx:claim(VIBRANCE, math.floor(p.vibrance + 0.5), { priority = FOCUS, fade = 300 })
    if ok then held[#held + 1] = string.format("vibrance %d %%", p.vibrance) else problems[#problems + 1] = err end
  else
    cx:release(VIBRANCE, { fade = 300 })
  end
  cx:publish({
    app = app,
    profile = p and p.exe,
    held = held,
    problems = problems,
    can_mode = cx:has(MODE),
    can_vibrance = cx:has(VIBRANCE),
    can_focus = cx:has(FOCUSED),
  })
end

function plugin.start(cx, s)
  cx:watch("app.focus")
  cx:watch("display.config")
  apply(cx, s)
end

function plugin.on(cx, s)
  apply(cx, s)
end

function plugin.stop(cx)
  cx:release_all({ fade = 300 })
end

-- ---- window ---------------------------------------------------------------

-- A GNOME Wayland session tells no application which window is in front.
local NO_FOCUS = "This session does not say which app is in front, so no profile can follow it."

function plugin.status(s, st)
  st = st or {}
  if st.can_focus == false then
    return { line = "Cannot follow apps", detail = NO_FOCUS, state = "failed" }
  end
  if st.problems and #st.problems > 0 then
    return { line = "Cannot apply", detail = st.problems[1], state = "failed" }
  end
  if st.profile and st.held and #st.held > 0 then
    return { line = st.profile, detail = table.concat(st.held, ", "), state = "holding" }
  end
  return {
    line = string.format("%d profiles", #s.profiles),
    detail = "Nothing to change for the app in front",
    state = "waiting",
  }
end

function plugin.page(ui, s, st)
  st = st or {}
  if st.can_focus == false then
    ui:card(nil, function(ui) ui:hint(NO_FOCUS) end)
    ui:space(12)
  elseif st.can_mode == false and st.can_vibrance == false then
    ui:card(nil, function(ui)
      ui:hint("This machine offers neither refresh rate nor vibrance control, so profiles do nothing here.")
    end)
    ui:space(12)
  end

  for i, p in ipairs(s.profiles) do
    local live = st.profile and string.lower(st.profile) == string.lower(p.exe or "")
    ui:card(nil, function(ui)
      ui:horizontal(function(ui)
        p.exe = ui:text(p.exe or "", { hint = "app.exe", width = 200 })
        if live then ui:chip("in front now", "bolt") end
        ui:right(function(ui)
          if ui:button("Remove", "close") then
            table.remove(s.profiles, i)
          end
        end)
      end)
      ui:space(6)
      ui:row("Refresh rate", "0 leaves it alone.", function(ui)
        p.refresh = ui:slider(p.refresh or 0, {
          min = 0, max = 360, step = 1,
          text = function(v) return v == 0 and "unchanged" or string.format("%d Hz", v) end,
        })
      end)
      ui:divider()
      ui:row("Vibrance", nil, function(ui)
        p.vibrance = ui:slider(p.vibrance or 50, {
          min = 0, max = 100, step = 1, preview = "display.vibrance",
          text = function(v) return string.format("%d %%", v) end,
        })
      end)
    end)
    ui:space(12)
  end

  ui:horizontal(function(ui)
    if ui:button("Add a profile", "add") then
      table.insert(s.profiles, { exe = "", refresh = 0, vibrance = 50 })
    end
    if st.app and not find(s, st.app) then
      if ui:button("Add " .. st.app, "game") then
        table.insert(s.profiles, { exe = st.app, refresh = 0, vibrance = 50 })
      end
    end
  end)
end

return plugin
