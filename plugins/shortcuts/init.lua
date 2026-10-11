-- Shortcuts: global key chords that run a program, type text, press other
-- keys or move the window in front. AutoHotkey's everyday use, with a page
-- instead of a script.

local plugin = {}

plugin.settings = {
  bindings = {
    { keys = "ctrl+alt+t", action = "run", target = "wt.exe" },
    { keys = "ctrl+alt+v", action = "type", target = "{clipboard}" },
    { keys = "win+alt+up", action = "window", target = "maximize" },
  },
}

local ACTIONS = { "run", "type", "send", "window" }
local ACTION_LABELS = {
  run = "Run a program",
  type = "Type text",
  send = "Press keys",
  window = "Window in front",
}
local WINDOW_ACTIONS = { "minimize", "maximize", "restore", "close" }

-- ---- daemon ---------------------------------------------------------------

local registered = {}

-- "C:\Program Files\x.exe" --flag "two words" -> program, { args }
local function split(command)
  local parts, rest = {}, command
  while true do
    rest = rest:gsub("^%s+", "")
    if rest == "" then break end
    if rest:sub(1, 1) == '"' then
      local close = rest:find('"', 2, true) or (#rest + 1)
      parts[#parts + 1] = rest:sub(2, close - 1)
      rest = rest:sub(close + 1)
    else
      local word = rest:match("^%S+")
      parts[#parts + 1] = word
      rest = rest:sub(#word + 1)
    end
  end
  return table.remove(parts, 1), parts
end

local function expand(cx, text)
  local minute = cx:now()
  text = text:gsub("{time}", function()
    if not minute then return "" end
    return string.format("%02d:%02d", math.floor(minute / 60), math.floor(minute) % 60)
  end)
  text = text:gsub("{clipboard}", function() return cx:clipboard() or "" end)
  return text
end

local function fire(cx, b)
  if b.action == "run" then
    local program, args = split(b.target or "")
    if program then cx:run(program, unpack(args)) end
    return true
  elseif b.action == "type" then
    return cx:type(expand(cx, b.target or ""))
  elseif b.action == "send" then
    return cx:send_keys(b.target or "")
  elseif b.action == "window" then
    for _, w in ipairs(cx:windows()) do
      if w.focused then return cx:window(w.id, b.target or "minimize") end
    end
    return nil, "no window has the focus"
  end
  return nil, "unknown action " .. tostring(b.action)
end

local function bind(cx, s)
  for _, id in ipairs(registered) do cx:unhotkey(id) end
  registered = {}
  local results = {}
  for i, b in ipairs(s.bindings) do
    if b.keys and b.keys ~= "" then
      local id, err = cx:hotkey(b.keys, function(cx, s)
        local ok, why = fire(cx, b)
        if not ok and why then cx:warn(b.keys .. ": " .. why) end
      end)
      if id then registered[#registered + 1] = id end
      results[i] = { ok = id ~= nil, error = err }
    else
      results[i] = { ok = false }
    end
  end
  cx:publish({ results = results })
end

function plugin.start(cx, s)
  bind(cx, s)
end

function plugin.on(cx, s, ev)
  if ev.kind == "settings" then bind(cx, s) end
end

function plugin.stop(cx)
  for _, id in ipairs(registered) do cx:unhotkey(id) end
  registered = {}
end

-- ---- window ---------------------------------------------------------------

function plugin.status(s, st)
  local bad = 0
  for _, r in ipairs((st or {}).results or {}) do
    if r.error then bad = bad + 1 end
  end
  if bad > 0 then
    return { line = string.format("%d refused", bad), detail = "Another app may own those keys", state = "failed" }
  end
  return {
    line = string.format("%d shortcuts", #s.bindings),
    detail = "Listening for them anywhere",
    state = #s.bindings > 0 and "holding" or "waiting",
  }
end

function plugin.page(ui, s, st)
  local results = (st or {}).results or {}
  for i, b in ipairs(s.bindings) do
    ui:card(nil, function(ui)
      ui:horizontal(function(ui)
        b.keys = ui:text(b.keys or "", { hint = "ctrl+alt+t", width = 150, mono = true })
        ui:space(6)
        b.action = ui:select(b.action or "run", ACTIONS, ACTION_LABELS)
        ui:right(function(ui)
          if ui:button("Remove", "close") then table.remove(s.bindings, i) end
        end)
      end)
      ui:space(6)
      if b.action == "window" then
        ui:row("Do", nil, function(ui)
          b.target = ui:select(b.target or "minimize", WINDOW_ACTIONS)
        end)
      else
        local hint = ({ run = "program.exe --flag", type = "text, {time}, {clipboard}", send = "ctrl+shift+esc" })[b.action]
        b.target = ui:text(b.target or "", { hint = hint, width = 0 })
      end
      local r = results[i]
      if r and r.error then
        ui:space(4)
        ui:label(r.error, { color = "failed", size = 12.5 })
      end
    end)
    ui:space(12)
  end
  if ui:button("Add a shortcut", "add") then
    table.insert(s.bindings, { keys = "", action = "run", target = "" })
  end
end

return plugin
