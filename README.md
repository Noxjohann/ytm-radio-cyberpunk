# YTM Radio

A YouTube Music radio station for Cyberpunk 2077. A small player window shows YouTube Music; what it plays is
streamed to a RadioExt station ("54.5 YTM Radio") in the game. The song name shows in the game's radio widget, and
play/pause/next/previous work from the CET overlay and from hotkeys.

Based on **Spot On Radio by Sz4bcsi (SZ4)**: the stream server and the RadioExt keep-alive logic come from it
(see `LICENSE-SpotOnRadio.txt`).

## Needs
Windows 10/11 with the Microsoft Edge WebView2 runtime (already on nearly every PC), Cyber Engine Tweaks, RadioExt.
RED4ext, Redscript and Codeware are **not** needed.

## Install
Copy the contents of this folder into the game folder (the `bin` folder merges with the one that is already there).
You get:
- `bin/x64/plugins/cyber_engine_tweaks/mods/YtmRadio/` with `init.lua` and `ytm-radio.exe`
- `bin/x64/plugins/cyber_engine_tweaks/mods/radioExt/radios/YtmRadio/metadata.json`

## Use
1. Start `ytm-radio.exe` (from the YtmRadio mod folder; a shortcut on your desktop is handy).
2. In its window, sign in if you want (optional) and play something.
3. In the game, tune to **54.5 YTM Radio**. The CET overlay has a "YTM Radio" window with the controls and settings.

The sound of the page does not come out of your speakers; it only plays through the game's radio.
Logs: `ytm-radio.log` next to the exe.
