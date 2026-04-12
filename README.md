<p align="center">
  <img src="assets/neowax-256.png" alt="neowax" width="128">
</p>

<h1 align="center">neowax</h1>

<p align="center">
  A vinyl timecode DJ player written in Rust.<br>
  Heavily inspired by <a href="https://xwax.org/">xwax</a>, neowax is a clean-room rewrite that aims to bring digital vinyl playback to any Linux box with ALSA audio and enough horsepower to keep up.
</p>

## Philosophy

neowax is built for DJs who spin vinyl and don't want to touch CDJs or controllers. The goal is an appliance-like experience: power it on, it works. Pull the plug, everything is fine. No updates, no configuration wizards, no network dependency.

Features are intentionally kept to a minimum. We want the experience to be as close to actual vinyl as possible. That means:

- No BPM display
- No loops or cue points
- No playlists or library management
- No sync buttons

Just two views: a **player** with a waveform showing playback position, and a **library** to pick the next track. That's it.

## How it works

neowax reads timecode signal from a vinyl record through an ALSA input device, decodes the signal to determine playback position and speed, and plays back a digital audio file through an ALSA output device in sync with the vinyl. Currently supports Serato 2A timecode.

## Supported audio formats

MP3, FLAC, WAV, OGG/Vorbis, AAC/M4A, AIFF, PCM

## Requirements

- Linux with ALSA
- An audio interface with line/phono input and output
- A turntable with Serato Control Vinyl (Serato 2A timecode)
- Rust toolchain to build from source

## Building

```sh
cargo build --release
```

## Usage

```sh
neowax-ui --alsa <device> --library <path-to-music>
```

### Options

| Option | Default | Description |
|---|---|---|
| `--alsa <device>` | | ALSA device for both input and output (e.g. `plughw:sndrpihifiberry`) |
| `--input-device <device>` | | ALSA input device (overrides `--alsa`) |
| `--output-device <device>` | | ALSA output device (overrides `--alsa`) |
| `-s, --sample-rate <hz>` | 48000 | Sample rate |
| `-t, --timecode <format>` | serato_2a | Timecode format |
| `--library, --crate <path>` | | Directory to scan for audio files |
| `--geometry <WxH>` | | Window size (e.g. `800x480`) |
| `--no-decor` | | Disable window decorations |
| `--speed <multiplier>` | 1.0 | Vinyl speed multiplier |
| `--phono` | | Enable phono-level input |
| `--buffer <frames>` | 256 | ALSA playback buffer size (~5.3ms at 48kHz) |
| `--capture-buffer <frames>` | 1024 | ALSA capture buffer size (~21ms at 48kHz) |
| `--capture-cpu <core>` | 3 | CPU core for the capture thread |
| `--playback-cpu <core>` | 2 | CPU core for the playback thread |

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE). You are free to use, modify, and distribute this software, provided that any derivative work is also released under the same license.
