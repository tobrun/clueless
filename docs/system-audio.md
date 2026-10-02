# System audio backends

The "Them" stream (everything the Mac plays that is not this app) can be captured three ways, selected by `audio.system_audio_backend` in the config file.
"Me" is always the microphone and is unaffected by this setting.

## The three backends

### `sck` (default) - ScreenCaptureKit

Captures the main display's audio through Apple's ScreenCaptureKit, with video reduced to 2x2 at 1 fps so only the audio matters, and `excludes_current_process_audio` on so the app never hears itself.
Delivers 48 kHz stereo float, resampled downstream like every other source.

- Permission needed: Screen Recording.
  The app preflights the grant at open, triggers the system prompt once on first failure, and reports "Grant Screen Recording in System Settings, then restart clueless" as a status error; the meeting keeps running with Me only.
- Caveat: macOS shows an orange recording indicator while this runs, and may re-ask for the grant after system updates or long gaps.

### `cpal_loopback` - Core Audio process tap on the default output

cpal 0.18 builds the input stream on the default output device through Apple's process-tap API, so no virtual device is needed.

- Permission needed: Microphone (the tap is presented as an input stream).
- Caveat: the tap has reported silent-failure cases after an unclean process exit; that is why it is not the default.
  Whether the tap survives on macOS 15.6.1 has not been verified on this Mac yet; the manual checklist (dev bundle run via `cargo xtask run`) has not been executed, so this page records no result.
  When the checklist is run, open a meeting with `system_audio_backend = "cpal_loopback"`, play audio, and confirm the Them stream carries samples; record the outcome here.

### `device:<name>` - a named input device (BlackHole and friends)

Opens the named cpal input device as the Them stream.
The error for an unknown name lists the available input devices.
Use this with a virtual audio device when the other two backends misbehave.

- Permission needed: Microphone.
- No recording indicator.

## BlackHole setup, step by step

1. Install BlackHole 2ch, e.g. `brew install blackhole-2ch`.
2. Open Audio MIDI Setup (Spotlight: "Audio MIDI Setup").
3. Menu: Add (+) -> Create Multi-Output Device.
4. In the new device's list, check your normal speakers/headphones and "BlackHole 2ch".
   Keep "Drift Correction" checked on BlackHole only.
5. Set the Multi-Output Device as the system output (its radio button in Audio MIDI Setup, or the Sound output pane, or the menu-bar volume slider).
   Audio now plays through the speakers *and* into BlackHole simultaneously.
6. In the clueless config set `system_audio_backend = "device:BlackHole 2ch"` (the name must match the input device name exactly; an unknown name in the config makes the app print the available input names).
7. Start a meeting; Them now receives whatever the Mac plays.
   Remember to switch the system output back to plain speakers when done, or other apps' volume control behaves oddly.

## Trying `cpal_loopback` on this Mac

Pending the manual checklist (dev certificate, first-launch permission grants, bundle run): once done, set `system_audio_backend = "cpal_loopback"`, play 30 s of audio, and note here whether the tap delivers samples and survives an unclean exit.
