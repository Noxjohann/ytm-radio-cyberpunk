## YTM Radio 0.1.0: first release

**YouTube Music as an in-game radio for Cyberpunk 2077. YouTube Music only.** A small player window shows YouTube
Music and its sound plays on the RadioExt station **54.5 YTM Radio**, with Artist - Title in the radio widget.

> **Made with Claude (AI disclosure):** this mod was written with Claude, an AI assistant by Anthropic, from my
> requests, testing and feedback. The code is open source so you can check it yourself.

**Based on Spot On Radio 1.4.0** by [Sz4bcsi (SZ4)](https://next.nexusmods.com/profile/Sz4bcs1)
([Nexus](https://www.nexusmods.com/cyberpunk2077/mods/34305?tab=description) ·
[GitLab](https://gitlab.com/Sz4bcsi/SpotOnRadio)). Please endorse the original.

### What it does
- YouTube Music in a small window; sign in (optional), pick songs or playlists there.
- Plays that sound through the game's radio only (not your speakers), following the game's radio volume.
- Artist - Title in the radio widget; play / pause / previous / next from the CET window and from hotkeys.
- Keeps working when the game pauses or a menu opens, with no need to press play again in the browser.
- **Loudness leveling** (on by default): brings every song to about the same loudness, with a limiter.
- **Mix button:** starts a YouTube Music mix from the song that is playing.

### Install
Needs Windows 10/11 (WebView2), Cyberpunk 2077, **Cyber Engine Tweaks** and **RadioExt** (no RED4ext, Redscript or
Codeware). Download `YtmRadio-v0.1.0.zip`, copy its `bin` folder into the game folder, run
`mods/YtmRadio/ytm-radio.exe`, and tune to **54.5 YTM Radio**. Windows may warn about an unsigned file the first
time: **More info > Run anyway**. Full instructions are in the README.

### Known limitations
- **Mix:** it reloads the page, and on later presses the page can show a **"Leave site?"** confirmation in the
  player window that you must confirm (Alt-Tab, click **Leave**) before the mix starts. Workaround: start the mix from
  the player window (song menu > Start radio) instead of the Mix button. Main thing to fix next.
- Mix is the song mix of the current song only.
- Non-Latin titles (e.g. Russian) may not display in the game's fonts.
- No album art. Windows only. The player is a separate program that must be running.

### Licence
MIT for the original code. If you reuse it, please credit **NoxJohann**
(<https://www.nexusmods.com/profile/NoxJohann>) and keep the credit to Sz4bcsi / Spot On Radio.
Third-party notices: `THIRD-PARTY-NOTICES.txt`.
