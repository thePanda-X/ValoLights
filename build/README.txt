ValoLights
==========

Finds your kills in Valorant recordings / replay extracts and turns them into highlight clips.
Everything runs on this PC; nothing is uploaded anywhere.

Getting started
---------------
1. Run ValoLights-Setup-<version>.exe. It needs an internet connection (it downloads about
   550 MB of video and GPU libraries) and no administrator rights. ValoLights is then in the Start menu.
2. Windows may show "Windows protected your PC" because the app isn't signed:
   click "More info" -> "Run anyway".
3. Enter your Riot name(s) (comma-separated if you have several accounts).
   If you play with "hide names", you appear as "Me" and that is detected automatically.
4. Click "Add video…" (or drag a video into the window). Analysis runs in the background.
5. Open the clip: trim, add subtitles, pick a format (16:9 / 9:16 / 1:1), then Export.

Requirements
------------
- Windows 10/11, 64-bit.
- An NVIDIA GPU with a recent driver is recommended (fast video decode/encode and speech-to-text).
  Everything also works on the CPU, just slower.
- The first time you use subtitles, the speech model (~0.6 GB) is downloaded into data\models.

Where things are
----------------
- data\config.toml     your settings
- data\projects\       one folder per analysed clip (your source videos are not copied)
- data\exports\        rendered videos and editor files
- data\music\          a handy place for music files
- data\logs\           logs (send these if something goes wrong)
- data\voice_lines.txt agent voice lines to hide from subtitles (one per line)

The installer puts everything in %LOCALAPPDATA%\Programs\ValoLights (the data folder is in there too).

Updating: ValoLights checks for updates when it starts and offers to install them (your data stays);
Settings -> Updates has "Check now" and an off switch. You can also run a newer installer by hand.

Source code, issues and releases: https://github.com/thePanda-X/ValoLights

Known limitations
-----------------
- English game client only.
- HUD layouts are tuned for the default HUD scale; very different HUD settings may reduce accuracy.
- Names in non-Latin scripts can't be read as text; pick yourself in "Who are you?" instead.
