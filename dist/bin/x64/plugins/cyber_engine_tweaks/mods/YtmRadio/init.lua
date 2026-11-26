-- YTM Radio: CET side. Talks to ytm-radio.exe (it lives in this same folder) through two small files:
--   state.txt  written by the app every 0.5 s: what is playing, whether the stream is connected
--   cmd.txt    written by this mod: next / prev / toggle / pause / play
-- (CET can only read and write files inside its own mod folder, which is why the app sits here.)
--
-- Based on Spot On Radio by Sz4bcsi (SZ4), see LICENSE-SpotOnRadio.txt. The RadioExt handling below (song name in the
-- radio widget, keeping the stream connected and muted while the radio is off, the fade-in, the loudness boost) is
-- that mod's logic, adapted. It relies on RadioExt internals (not a public API), so everything touching them is in pcall.
--
-- RadioExt notes (from Spot On Radio, checked against RadioExt 2.81):
--  * The widget text comes from station.currentSong.path; station:tryUpdateUI() raises the game's own "song changed" events.
--  * RadioExt starts a stream with RadioExt.Play(-1, url, -1, volume, fade) inside station:activate() and kills it with
--    RadioExt.Stop(-1) in station:deactivate(); every start replays the first ~0.5 s (glitch). We shadow activate/deactivate
--    on OUR station object (channel -1 only): "off" = RadioExt.SetVolume(-1, 0) (stream stays connected), "on" = unmute with a fade.
--  * Loudness: RadioExt multiplies the stream by 0.4 on top of the game slider, so the boost scales the station volume
--    (250% = source level).

-- Must match STREAM_ADDR in the app and streamInfo.streamURL in radios/YtmRadio/metadata.json.
local STREAM_URL = 'http://127.0.0.1:52848/stream.mp3'
local RAMP_SECONDS = 1.5       -- fade-in when the radio comes back on
local PRESTART_COOLDOWN = 10   -- seconds between attempts to connect the stream in the background
local UNTUNED_SECONDS = 2      -- radio off this long before the music pauses (not while scrolling past stations)
local BOOST_MIN, BOOST_MAX = 100, 250
local LOST_SECONDS = 5         -- a stream nobody is reading for this long counts as lost
local ALIVE_SECONDS = 3        -- no new state.txt for this long = the app is not running

local Radio = {
    st = nil,           -- last parsed state
    alive = false,      -- ytm-radio.exe is running
    lastTick = nil,
    lastTickAt = -100,
    open = false,       -- CET overlay visible
    uptime = 0,
    poll = 0,
    shownSeq = 0,       -- last song we announced
    linked = nil,       -- nil = not tried yet, true/false = last try to reach the RadioExt station
    keepAlive = true,   -- keep the stream connected (muted) while the radio is off
    hooked = nil,       -- our RadioExt station object once activate/deactivate are shadowed
    manager = nil,
    baseVolume = 1,     -- station volume x boost
    stationVolume = 1,
    boost = 100,        -- loudness boost in percent (100 = RadioExt default)
    boostFile = 'boost.txt',
    boostDirty = nil,
    autoPause = true,   -- pause the music while the game is paused, in a scene/call, or the radio is off
    autoPauseFile = 'autopause.txt',
    paused = false,     -- auto-pause is in effect right now
    pausedByUs = false, -- the app was playing when we paused it (so we resume only that)
    flags = {},
    untunedSince = nil,
    parked = false,     -- channel -1 holds our stream (connected or still connecting), audible or muted
    audible = false,    -- the radio is on (the parked stream is unmuted)
    lastHeard = 0,
    ramp = nil,
    lastPrestart = -100,
    cmdCounter = 0,
}

-- ---------------------------------------------------------------- pure helpers

local function split(s, sep)
    local out, i = {}, 1
    while true do
        local j = string.find(s, sep, i, true)
        if not j then
            out[#out + 1] = string.sub(s, i)
            return out
        end
        out[#out + 1] = string.sub(s, i, j - 1)
        i = j + #sep
    end
end

-- state.txt: v1 playing seq title artist album clients stream_down tick (tab separated)
function Radio.parseState(line)
    if type(line) ~= 'string' then return nil end
    line = string.gsub(line, '[\r\n]+$', '')
    local f = split(line, '\t')
    if #f < 9 or f[1] ~= 'v1' then return nil end
    return {
        playing = f[2] == '1', seq = tonumber(f[3]) or 0, title = f[4], artist = f[5], album = f[6],
        clients = tonumber(f[7]) or 0, streamDown = f[8] == '1', tick = tonumber(f[9]) or 0,
    }
end

function Radio.shouldPause(f)
    return (f.gamePaused or f.scene or f.untuned) and true or false
end

-- Text for the radio widget, like RadioExt's own file stations ("Artist - Title").
function Radio.trackText(st)
    if not st or st.title == '' then return nil end
    if st.artist ~= '' then return st.artist .. ' - ' .. st.title end
    return st.title
end

-- ---------------------------------------------------------------- files shared with the app

local function readFile(name)
    local ok, text = pcall(function()
        local f = io.open(name, 'r')
        if not f then return nil end
        local t = f:read('*a')
        f:close()
        return t
    end)
    if ok then return text end
    return nil
end

local function writeFile(name, text)
    return pcall(function()
        local f = io.open(name, 'w')
        f:write(text)
        f:close()
    end)
end

-- The app runs a command when the file content changes, so every command is different from the last one.
local function sendCmd(name)
    Radio.cmdCounter = Radio.cmdCounter + 1
    writeFile('cmd.txt', string.format('%d:%.3f:%s', Radio.cmdCounter, Radio.uptime, name))
end

-- ---------------------------------------------------------------- game side

-- Our RadioExt station object, found by its stream URL (so renaming the station is fine).
-- RadioExt loads after this mod, so this is resolved lazily and may be nil at first.
function Radio.station()
    local ok, station, manager = pcall(function()
        local radioExt = GetMod('radioExt')
        local manager = radioExt and radioExt.radioManager
        if not manager then return nil end
        for _, radio in pairs(manager.radios) do
            local info = radio.metadata and radio.metadata.streamInfo
            if info and info.streamURL == STREAM_URL then return radio, manager end
        end
        return nil
    end)
    if not ok then return nil end
    return station, manager
end

-- Show `text` where the game shows the song for every other radio.
function Radio.announce(text)
    local radio = Radio.station()
    Radio.linked = false
    if radio and pcall(function()
        radio.currentSong = { path = text, length = 0 }
        radio:tryUpdateUI()
    end) then
        Radio.linked = true
    end
    if not Radio.linked and not Radio.warned then
        Radio.warned = true
        print('[YtmRadio] RadioExt station not reachable yet (is RadioExt installed and the station folder present?)')
    end
    return Radio.linked
end

local function radioExtCall(name, ...)
    return (pcall(function(...) return RadioExt[name](...) end, ...))
end

local function note(msg)
    print(string.format('[YtmRadio] %.1fs %s', Radio.uptime, msg))
end

-- Channel -1 is shared by every RadioExt station: never touch it while another station owns it.
local function otherOwnsChannel(manager, station)
    if not manager then return false end
    for _, other in pairs(manager.radios) do
        if other ~= station and other.channels and other.channels[-1] then return true end
    end
    return false
end

function Radio.inGame()
    if Radio.inGameNow == nil then
        local ok, player = pcall(function() return Game.GetPlayer() end)
        Radio.inGameNow = ok and player ~= nil
    end
    return Radio.inGameNow
end

-- Shadow activate/deactivate on our station object (channel -1 only). Returns false, touching nothing, if
-- RadioExt does not look like the version this was written for.
function Radio.hook(station, manager)
    if Radio.hooked == station then return true end
    local class = getmetatable(station)
    local base = class and class.__index
    local activate = type(base) == 'table' and base.activate
    local deactivate = type(base) == 'table' and base.deactivate
    if type(activate) ~= 'function' or type(deactivate) ~= 'function' or type(station.channels) ~= 'table' then
        if not Radio.hookWarned then
            Radio.hookWarned = true
            note('keep-alive off: this RadioExt version has no activate/deactivate to shadow')
        end
        return false
    end
    Radio.hooked, Radio.manager, Radio.stationVolume = station, manager, station.volume or 1
    Radio.baseVolume = Radio.stationVolume * Radio.boost / 100
    station.volume = Radio.baseVolume
    Radio.parked, Radio.audible, Radio.ramp, Radio.lastHeard = station.channels[-1] == true, station.channels[-1] == true, nil, Radio.uptime
    local okv, version = pcall(function() return tostring(RadioExt.GetVersion()) end)
    note('keep-alive on (RadioExt ' .. (okv and version or '?') .. ')')

    station.deactivate = function(self, channel)
        if channel ~= -1 or not Radio.keepAlive then return deactivate(self, channel) end
        if not self.channels[-1] then return end
        self.channels[-1] = false
        Radio.ramp, self.volume = nil, Radio.baseVolume
        -- "Off" = quiet, not dead: the stream stays connected, so switching on again is only an unmute.
        if Radio.parked and not otherOwnsChannel(manager, self) then radioExtCall('SetVolume', -1, 0) end
        -- RadioExt's menu handling calls this every frame; say it once.
        if Radio.audible then note('radio off: muted, stream kept') end
        Radio.audible = false
    end

    station.activate = function(self, channel, updateUI)
        if channel ~= -1 or not Radio.keepAlive then return activate(self, channel, updateUI) end
        if self.channels[-1] then return end
        Radio.audible = true
        if Radio.parked then
            -- Never call RadioExt's activate here: it would start a new stream (= the start glitch).
            self.channels[-1] = true
            Radio.ramp = { t = 0 }
            self.volume = 0 -- RadioExt scales this by the game's radio volume; Radio.stepRamp raises it
            pcall(self.updateVolume, self, -1)
            if updateUI ~= false then pcall(self.tryUpdateUI, self) end
            note('radio on: unmuted parked stream')
            return
        end
        -- No parked stream (first tune-in before it was connected, or it was lost): the normal way.
        Radio.ramp, self.volume, Radio.parked, Radio.lastHeard = nil, Radio.baseVolume, true, Radio.uptime
        note('radio on: no parked stream, RadioExt starts it')
        return activate(self, channel, updateUI)
    end
    return true
end

-- Fade the volume back in after an unmute (quadratic).
function Radio.stepRamp(dt)
    local ramp, station = Radio.ramp, Radio.hooked
    if not ramp or not station or Radio.paused then return end
    if not station.channels[-1] then
        Radio.ramp, station.volume = nil, Radio.baseVolume
        return
    end
    ramp.t = ramp.t + dt
    local x = math.min(ramp.t / RAMP_SECONDS, 1)
    station.volume = Radio.baseVolume * x * x
    pcall(station.updateVolume, station, -1)
    if x >= 1 then Radio.ramp = nil end
end

-- Runs on every poll while the app is running: connect the stream muted once we are in the world (so even the first
-- tune-in is only an unmute), notice a lost stream, and keep the channel volume matching the radio state.
function Radio.keepStreamWarm(st)
    if not Radio.keepAlive then return end
    local station, manager = Radio.station()
    if not station or not Radio.hook(station, manager) then return end
    local on = station.channels[-1] == true

    if otherOwnsChannel(manager, station) then -- its Play replaced our stream on the shared channel
        if Radio.parked then
            Radio.parked, Radio.audible = false, false
            note('car channel taken by another station')
        end
        return
    end

    if st.clients > 0 then Radio.lastHeard = Radio.uptime end
    if Radio.parked and Radio.uptime - Radio.lastHeard > LOST_SECONDS then
        Radio.parked = false
        note('stream lost, reconnecting')
        if on then -- the radio is on: restart it the normal way (RadioExt's own fade)
            station.channels[-1], Radio.audible = false, false
            station:activate(-1, false)
            return
        end
    end

    if not Radio.parked then
        if on or st.streamDown or not Radio.inGame() or Radio.uptime - Radio.lastPrestart < PRESTART_COOLDOWN then return end
        Radio.lastPrestart = Radio.uptime
        if radioExtCall('Play', -1, STREAM_URL, -1, 0, 0.75) then -- volume 0: connected, silent
            Radio.parked, Radio.audible, Radio.lastHeard = true, false, Radio.uptime
            note('stream parked (connected, muted)')
        end
        return
    end

    -- The stream may still have been loading when the radio was switched on or off (a SetVolume then hits no
    -- channel), so keep the volume in line with the state.
    if not on then
        radioExtCall('SetVolume', -1, 0)
    elseif not Radio.ramp then
        pcall(station.updateVolume, station, -1)
    end
end

function Radio.setKeepAlive(value)
    Radio.keepAlive = value
    local station = Radio.hooked
    if not station then return end
    local on = station.channels[-1] == true
    if value then
        Radio.parked, Radio.audible, Radio.lastHeard = on, on, Radio.uptime
        return
    end
    -- Switched off while a muted stream is parked: release it now, nothing else will.
    if Radio.parked and not on and not otherOwnsChannel(Radio.manager, station) then radioExtCall('Stop', -1) end
    Radio.parked, Radio.audible, Radio.ramp = false, false, nil
    station.volume = Radio.baseVolume
    if on then pcall(station.updateVolume, station, -1) end
end

local function try(fn)
    local ok, v = pcall(fn)
    return ok and v or nil
end

-- Game reads for Radio.shouldPause. All in pcall: a changed game API just means "no".
-- Note (from Spot On Radio): SceneTier >= 3 (dialogue scenes, calls) is an unverified guess, untested in game.
local function readGamePaused()
    return try(function() return Game.GetSystemRequestsHandler():IsGamePaused() end) == true
end

local function readScene()
    return try(function()
        local defs = Game.GetAllBlackboardDefs()
        local bb = Game.GetBlackboardSystem():GetLocalInstanced(Game.GetPlayer():GetEntityID(), defs.PlayerStateMachine)
        return bb:GetInt(defs.PlayerStateMachine.SceneTier) >= 3
    end) == true
end

-- Every frame (instant mute when the game pauses); `poll` also refreshes the slower flags. Acts on the edge only:
-- pauses the music when it was playing, and resumes only what this paused (a manual play/pause is never fought).
function Radio.updatePause(poll)
    local f, station = Radio.flags, Radio.hooked
    local inGame = Radio.inGame()
    f.gamePaused = inGame and readGamePaused()
    if poll then
        f.scene = inGame and readScene()
        if inGame and station and not station.channels[-1] then
            Radio.untunedSince = Radio.untunedSince or Radio.uptime
        else
            Radio.untunedSince = nil
        end
        f.untuned = Radio.untunedSince ~= nil and Radio.uptime - Radio.untunedSince >= UNTUNED_SECONDS
    end
    local want = Radio.autoPause and Radio.shouldPause(f)
    if want == Radio.paused then return end
    Radio.paused = want
    if want then
        -- Instantly silent but connected (cutting the stream would play a bad sound effect).
        if station then
            station.volume = 0
            pcall(station.updateVolume, station, -1)
        end
        if Radio.alive and Radio.st and Radio.st.playing then
            Radio.pausedByUs = true
            sendCmd('pause')
        end
        note('auto-pause')
    else
        note('auto-pause over')
        if Radio.pausedByUs then
            Radio.pausedByUs = false
            if Radio.alive then sendCmd('play') end
        end
        if station then
            if station.channels[-1] then
                if not Radio.ramp then
                    Radio.ramp, station.volume = { t = 0 }, 0
                    pcall(station.updateVolume, station, -1)
                end
            else
                station.volume = Radio.baseVolume
            end
        end
    end
end

function Radio.loadAutoPause()
    local text = readFile(Radio.autoPauseFile)
    if text then Radio.autoPause = tonumber(text) ~= 0 end
end

function Radio.setAutoPause(value)
    Radio.autoPause = value
    writeFile(Radio.autoPauseFile, value and '1' or '0')
end

function Radio.loadBoost()
    local value = tonumber(readFile(Radio.boostFile))
    if value then Radio.boost = math.max(BOOST_MIN, math.min(BOOST_MAX, math.floor(value))) end
end

function Radio.saveBoost()
    Radio.boostDirty = nil
    local ok = writeFile(Radio.boostFile, tostring(Radio.boost))
    note(ok and ('loudness boost ' .. Radio.boost .. '% saved') or 'could not save the loudness boost')
end

-- Louder (or back to normal) right away, also mid-fade; saved a moment after the last change.
function Radio.setBoost(pct)
    pct = tonumber(pct)
    if not pct then return end
    pct = math.max(BOOST_MIN, math.min(BOOST_MAX, math.floor(pct + 0.5)))
    if pct == Radio.boost then return end
    Radio.boost, Radio.boostDirty = pct, Radio.uptime
    local station = Radio.hooked
    if not station then return end
    Radio.baseVolume = Radio.stationVolume * pct / 100
    if Radio.ramp then return end -- stepRamp aims at the new baseVolume
    station.volume = Radio.baseVolume
    if station.channels[-1] then pcall(station.updateVolume, station, -1) end
end

-- Read state.txt: the app is running as long as its tick keeps changing.
function Radio.readState()
    local st = Radio.parseState(readFile('state.txt'))
    if not st then
        Radio.alive = false
        return nil
    end
    if Radio.lastTick == nil then
        Radio.lastTick = st.tick -- a file left over from an earlier run says nothing yet
    elseif st.tick ~= Radio.lastTick then
        Radio.lastTick, Radio.lastTickAt = st.tick, Radio.uptime
    end
    Radio.alive = Radio.uptime - Radio.lastTickAt <= ALIVE_SECONDS
    return st
end

function Radio.refresh()
    if Radio.boostDirty and Radio.uptime - Radio.boostDirty > 1 then Radio.saveBoost() end
    local st = Radio.readState()
    Radio.st = st
    Radio.updatePause(true)
    if not st or not Radio.alive then return end
    Radio.keepStreamWarm(st)

    -- Once per song (also while paused, so the widget is right when you tune in). Retry until RadioExt is up.
    if st.seq ~= Radio.shownSeq then
        local text = Radio.trackText(st)
        if text == nil or Radio.announce(text) then Radio.shownSeq = st.seq end
    end
end

function Radio.draw()
    local st = Radio.st
    if not st or not Radio.alive then
        ImGui.TextWrapped('The YTM Radio app is not running. Start ytm-radio.exe (it is in this mod folder: '
            .. 'bin/x64/plugins/cyber_engine_tweaks/mods/YtmRadio) and pick something to play in its window.')
        return
    end
    if st.streamDown then
        ImGui.TextWrapped('NO AUDIO: port 52848 is already in use. Is a second copy of YTM Radio running? '
            .. 'Close it and start the app again.')
        ImGui.Separator()
    end
    if Radio.linked == true then
        ImGui.Text('Song names: shown in the game radio widget')
    elseif Radio.linked == false then
        ImGui.TextWrapped('Song names: RadioExt station not found (is RadioExt installed and updated?)')
    end
    ImGui.Text('Stream: ' .. (st.clients > 0 and (Radio.keepAlive and Radio.parked and not Radio.audible and 'connected, muted while the radio is off' or 'connected') or 'not connected yet'))
    local keep, pressed = ImGui.Checkbox('Keep the stream running when the radio is off', Radio.keepAlive)
    if pressed then Radio.setKeepAlive(keep) end
    local ap, apPressed = ImGui.Checkbox('Pause the music when the game pauses, in scenes and calls, or when the radio is off', Radio.autoPause)
    if apPressed then Radio.setAutoPause(ap) end
    ImGui.Separator()
    if st.title ~= '' then
        ImGui.Text(st.title)
        ImGui.TextWrapped(st.artist .. (st.album ~= '' and ('   |   ' .. st.album) or ''))
    else
        ImGui.Text('Nothing playing.')
    end
    ImGui.Spacing()

    if ImGui.Button('  |<  ##prev') then sendCmd('prev') end
    ImGui.SameLine()
    if ImGui.Button(st.playing and '  Pause  ##pp' or '  Play  ##pp') then sendCmd('toggle') end
    ImGui.SameLine()
    if ImGui.Button('  >|  ##next') then sendCmd('next') end

    ImGui.Text("Volume: use the game's radio volume slider")
    local boost, moved = ImGui.DragInt('Loudness boost', Radio.boost, 1, BOOST_MIN, BOOST_MAX, '%d%%')
    if moved then Radio.setBoost(boost) end
    ImGui.TextWrapped('On top of the game slider. 250% is the loudness of the music itself.')
end

-- ---------------------------------------------------------------- CET wiring

-- Hotkeys must be registered at the root of init.lua. Unbound by default: set keys in CET > Bindings.
registerInput('ytm_playpause', 'YTM Radio: Play / Pause', function(down) if down then sendCmd('toggle') end end)
registerInput('ytm_next', 'YTM Radio: Next track', function(down) if down then sendCmd('next') end end)
registerInput('ytm_prev', 'YTM Radio: Previous track', function(down) if down then sendCmd('prev') end end)

registerForEvent('onInit', function()
    Radio.loadBoost()
    Radio.loadAutoPause()
    print('[YtmRadio] ready')
end)

registerForEvent('onOverlayOpen', function() Radio.open = true end)
registerForEvent('onOverlayClose', function() Radio.open = false end)

registerForEvent('onUpdate', function(dt)
    Radio.uptime = Radio.uptime + dt
    Radio.inGameNow = nil
    Radio.stepRamp(dt)
    Radio.updatePause(false)
    Radio.poll = Radio.poll + dt
    if Radio.poll < 0.5 then return end
    Radio.poll = 0
    Radio.refresh()
end)

registerForEvent('onDraw', function()
    if not Radio.open then return end
    ImGui.SetNextWindowSize(440, 300, 4) -- 4 = ImGuiCond_FirstUseEver
    if ImGui.Begin('YTM Radio') then Radio.draw() end
    ImGui.End()
end)

return Radio
