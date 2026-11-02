# Horizon Voice

Proximity voice chat for [Horizon](https://playhorizon.dev), as a native mod. It speaks the network
protocol of [Simple Voice Chat](https://modrinth.com/plugin/simple-voice-chat), so a Horizon player can
talk with Java Edition players on any server that runs Simple Voice Chat 2.6.x.

**This is an independent implementation. It is not made by, affiliated with or endorsed by the authors
of Simple Voice Chat, and it contains none of their code.** Its wire format was worked out by reading
their public source, whose licence is "All Rights Reserved". The server still needs their plugin or mod.

## Features

- Push to talk (V) or voice activation; mute (M); menu (G). Keys can be changed in Options > Mods.
- Volume by distance and stereo panning by where the speaker stands.
- Groups: join and leave from the menu, or `/vgroup list | join | create | leave`.
- Noise suppression, microphone gain, volume, choice of devices, and a microphone test.
- English and Russian, following the game's language.

Settings are kept in `config/horizon_voice.txt`.

## Status

Tested on Windows, on Paper 26.3 with Simple Voice Chat 2.6.25 and on a public server: connecting and
voice in both directions, creating and listing groups. Not tested yet: Linux and macOS, the menu key,
voice inside a group with a Java player, and Simple Voice Chat 2.7 (`compatibility_version=21`).

The mod hooks `Game::handle_packet`, which is not one of Horizon's stable hook points, so it is pinned to
one Horizon revision (see `mod/horizon-mixin.toml`) and needs updating for newer ones.

Native mods are not sandboxed: this one uses the microphone and opens a UDP connection to the voice
server the Minecraft server names.

## Layout

- `mod/`: the mixin manifest, one-line hook helpers and the glue module (`runtime.rs`).
- `engine/`: the mod's crate: protocol, encryption, UDP, Opus, sound, noise suppression, settings.
- `engine/src/opus/`: [opus-rs](https://github.com/restsend/opus-rs) 0.1.40, vendored by
  `tools/vendor_opus.py` because a native mod may only depend on crates in Horizon's `Cargo.lock`.
- `tools/package.py OUT_DIR`: builds `horizon_voice.hnmod`.

## Licence

MIT (see `LICENSE`), except `engine/src/opus/`, which is BSD-3-Clause (its licence heads `engine/src/opus/mod.rs`).
