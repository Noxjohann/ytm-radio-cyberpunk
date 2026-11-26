# YTM Radio: YouTube Music as an in-game radio for Cyberpunk 2077

**This mod works with YouTube Music only.** It is not a Spotify mod, and it is not a general "play any app" mod.
It shows the YouTube Music web player in a small window and plays whatever it plays through a station in the
game's radio (RadioExt): **54.5 YTM Radio**.

> ## Made with Claude (AI disclosure)
> This mod was written with **Claude**, an AI assistant made by Anthropic, working from my requests, my testing and
> my feedback. The code, this README and the release notes were produced that way. I (NoxJohann) chose what to build,
> tested every version in the game, and decided what to keep. I am saying it plainly so nobody has to wonder: the
> code was AI-generated, it works for me, and it may have bugs I have not met yet. The full source is in this
> repository, so you can read exactly what it does before running it.

## Credits: based on Spot On Radio

YTM Radio is **based on [Spot On Radio](https://www.nexusmods.com/cyberpunk2077/mods/34305?tab=description) by
[Sz4bcsi (SZ4)](https://next.nexusmods.com/profile/Sz4bcs1)**, version **1.4.0**
(source: <https://gitlab.com/Sz4bcsi/SpotOnRadio>). Spot On Radio is the "your Spotify (or any music app) as an
in-game radio (RadioExt)" mod. From it I took the MP3 stream server and the RadioExt keep-alive and song-widget
logic, and replaced the Spotify side with YouTube Music. Spot On Radio's licence asks for credit to its creator,
and this is that credit: **go and endorse the original.** The licence text is in `LICENSE-SpotOnRadio.txt`.

Also required (not included, not modified): **Cyber Engine Tweaks** by yamashi and **RadioExt** by keanuWheeze.

## What it does

- Shows **YouTube Music** in a small window (`ytm-radio.exe`). You sign in (optional) and pick songs or playlists
  there, like in any browser.
- Sends the sound of that page to the game as a radio station, **54.5 YTM Radio**. The sound does **not** come out of
  your speakers by itself; it is only heard through the game's radio, so it follows the game's radio volume.
- Shows **Artist - Title** in the game's radio widget.
- Gives you a **CET window** with play / pause / previous / next, a **Mix** button, and a few settings. The same
  actions can be bound to hotkeys in CET > Bindings (under this mod).
- **Keeps working when the game pauses or a menu opens.** You do not need to switch to the browser and press play
  again to get the sound back.
- **Evens out the loudness between songs** (optional, on by default): a leveler measures how loud the music is and
  brings every song to about the same level, with a soft limiter against clipping. Turn it off in the CET window.
- **Mix button:** starts YouTube Music's mix (radio) from the song that is playing, without leaving the game. See the
  limitation below.

## Requirements

- Windows 10 or 11 (the player uses Microsoft Edge WebView2, already installed on nearly every PC).
- Cyberpunk 2077 with **Cyber Engine Tweaks** and **RadioExt**.
- YTM Radio itself adds no RED4ext, Redscript or Codeware plugin and needs none of them. (RadioExt has its own
  requirements, such as RED4ext: follow the instructions on its page.)

## Install

1. Download `YtmRadio-vX.Y.Z.zip` from the [Releases](../../releases) page.
2. Extract it and copy the `bin` folder into your Cyberpunk 2077 folder (it merges with the existing `bin`).
   You get:
   - `bin/x64/plugins/cyber_engine_tweaks/mods/YtmRadio/` with `init.lua` and `ytm-radio.exe`
   - `bin/x64/plugins/cyber_engine_tweaks/mods/radioExt/radios/YtmRadio/metadata.json`
3. The first time you run `ytm-radio.exe`, Windows may say "Windows protected your PC" because the file is not
   code-signed. Choose **More info > Run anyway**. The source and the build are public (see below).

## How to use

1. Start `ytm-radio.exe` (from the `YtmRadio` mod folder; a desktop shortcut is handy) **before or while** playing.
2. In its window, sign in if you want and start something playing.
3. In the game, tune the radio to **54.5 YTM Radio**.
4. Open the CET overlay for the **YTM Radio** window: controls, Mix, loudness leveling, settings.
5. Keep the player window open (minimizing it is fine). Closing it stops the station.

Use the game's radio volume slider for the volume.

## Known limitations

- **The Mix button can ask for a confirmation.** The mix starts by reloading the YouTube Music page. The first mix
  usually starts cleanly, but on later presses the page may show a **"Leave site?"** confirmation in the player window.
  Until it is confirmed (Alt-Tab to the player window and click **Leave**), the new mix does not start and the music
  in the game does not switch. I tried to suppress that prompt and did not manage to; it is the main thing to fix.
  Workaround: start the mix from the player window itself (the song's menu > Start radio) instead of the Mix
  button, or confirm the prompt when it appears.
- **Mix is the song mix only:** it starts the YouTube Music mix of the *current song*.
- **Non-Latin titles** (for example Russian) are passed on unchanged, but whether they *display* depends on the
  font of whatever shows them (the game's radio widget, or the CET window). They may show as `?` or empty boxes.
- **No album art.** CET cannot show images, and the radio widget only has the station's fixed icon.
- **YouTube Music only**, **Windows only**, and a **free account** plays ads like in any browser (the leveler
  also evens those out).
- The player is a separate program that has to be running. CET cannot start it for you.
- Not code-signed: SmartScreen or an antivirus may warn about it the first time.

## Troubleshooting

- The app writes `ytm-radio.log` next to `ytm-radio.exe` (the previous run is kept as `ytm-radio.old.log`). If
  something does not work, that file says what happened.
- "NO AUDIO: port 52848 is already in use" in the CET window: another copy of the player is already running.
- No station in the radio: check that the `radios/YtmRadio/metadata.json` file is in place and that RadioExt works.
- Another mod uses 54.5: change `fm` in `metadata.json`.

## Remove it

Close `ytm-radio.exe`, then delete `mods/YtmRadio/` and `mods/radioExt/radios/YtmRadio/`. Optionally delete
`%APPDATA%\YtmRadio` (the player's browser profile, with your sign-in). Nothing else on your PC or in your saves is
changed; the mod adds no game files outside those two folders.

## What it does *not* do

It does not download music, record audio to disk, or send anything anywhere except the local stream on your own PC
(`127.0.0.1:52848`). YouTube Music itself talks to YouTube like in any browser.

## Building from source

`cargo build --release` on Windows (Rust stable). `cargo test` runs the unit tests. GitHub Actions builds every push
(`.github/workflows/build.yml`); a tag like `v0.1.0` also publishes a Release.

## Licence and reuse

The code in this repository is open source under the **MIT licence** (`LICENSE`), so you may use, change and share
it. **If you use my code, please credit me (NoxJohann) and link my Nexus profile:
<https://www.nexusmods.com/profile/NoxJohann>**, and keep the credit to **Sz4bcsi / SZ4 and Spot On Radio** that
comes with the parts derived from it (`LICENSE-SpotOnRadio.txt`). Third-party components keep their own licences, see
`THIRD-PARTY-NOTICES.txt` (notably shine-rs, LGPL-2.0).

YTM Radio is a fan-made mod. It is not affiliated with or endorsed by Google, YouTube, CD PROJEKT RED, Anthropic or
the author of Spot On Radio.
