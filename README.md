# Super Sekiro 64 (sk-mario)

Play Sekiro: Shadows Die Twice as Mario, with Super Mario 64's real movement: triple jumps, wall
kicks, long jumps, ground pounds, punches and kicks that hurt Sekiro's enemies, deathblows,
Bowser's tail swing on bosses, the grappling hook, SM64's health meter and Lakitu's camera.

Work in progress. You need your own Super Mario 64 ROM (US version): Mario's model, textures,
sounds and animations come from it. Nothing of Nintendo's or FromSoftware's is included.

## A fork of ER Mario

This is a fork of [deltarooo/er-mario](https://github.com/deltarooo/er-mario), the Elden Ring mod
that puts Super Mario 64's Mario into Elden Ring. Its idea and much of its code (libsm64 running
Mario in the game's process, the overlay HUD, the ROM loading, Lakitu's camera) are kept. All of the
game-facing parts had to be redone for Sekiro, a different engine generation with its own memory
layout and no Rust bindings comparable to Elden Ring's:

- **Loading:** me3 loads the DLL into Sekiro with its Arxan protection disabled
  (`disable_arxan` in `sk-mario.me3`). The mod's own code runs every frame through Sekiro's task
  system.
- **Finding the game:** the player, cameras, Havok world, bullet manager, boss bars, menus and
  popups were all found again in Sekiro 1.06 (addresses and offsets are documented at the top of
  each source file).
- **Drawing Mario:** Mario's mesh is drawn with Sekiro's own debug triangle renderer, filled and
  lit from the scene around him, instead of Elden Ring's.
- **Collision:** Mario walks on Sekiro's live Havok collision, read straight from the physics
  world: meshes and water layers. The game's invisible walls are filtered out,
  and the triangles are mirrored and flipped into SM64's coordinates.
- **Sekiro stays underneath:** the real character is hidden and pinned to Mario, so idols, doors,
  saving and dying keep working. The game can still take over its character for the grappling
  hook, long falls and deathblows, and Mario rides along.
- **Combat:** Mario's punches, dives, ground pounds and boss throws fire the game's own bullets
  carrying Sekiro's sword attack rows. They go through Sekiro's damage pipeline, so they deal
  damage and posture damage and cause hit reactions like the sword. On top of that:
  - Bosses guard most of Mario's hits, but some get through and take HP, like Sekiro's sword does.
  - A damage multiplier can be changed in game.
  - A punch triggers a deathblow when Sekiro offers one.
  - Auto-aim follows Sekiro's lock-on, bosses included.
  - A punch on a boss from behind can grab him by the tail, Bowser style.
- **Controls:** a gamepad hook splits the pad between Mario and the game: B is Sekiro's grappling
  hook, LB deflects, Y is the action button, and the rest is Mario's. **F8 / Select** switches
  between Mario and plain Sekiro, where the default controls are back.
- **Look:** a "Super Sekiro 64" title logo, built in from your own game files, and SM64's music on
  F7.
- **Removed from ER Mario:** features that only made sense in Elden Ring, such as Torrent,
  graces, the update check and coins.

## Installing

You need:
- Sekiro on Steam (version 1.06)
- [me3](https://me3.help), the mod loader
- a Super Mario 64 ROM (US version, `.z64`)

Never play online with the mod. me3 starts the game offline.

### The easy way: a prebuilt release

1. Install **me3** from [me3.help](https://me3.help).
2. Download the latest **SK-Mario** zip from this repository's
   [Releases](https://github.com/BassGaming/super-sekiro-64/releases) page and unzip it somewhere you can
   write to, such as your Documents folder. It contains `sk_mario.dll`, `sk-mario.me3`,
   `sk_mario.ini` and an empty `sk_mario_title` folder.
3. Put your Super Mario 64 ROM into that folder. The mod uses the first `.z64`, `.n64` or `.v64`
   file it finds there, or the path set with `rom = ...` in `sk_mario.ini`.
4. Double-click **sk-mario.me3** to start Sekiro with the mod.

On the first start the mod builds the title logo (see [The title logo](#the-title-logo)), which
can show from the second start on.

A loaded savegame starts in plain Sekiro mode, and the screen says **Press F1 for controls/help**.
Press **F8** (pad: **Select**) to become Mario.

### Building it yourself

On Windows you need:
- [Rust](https://rustup.rs)
- Visual Studio Build Tools (C++)
- [LLVM](https://github.com/llvm/llvm-project/releases), whose clang-cl compiles libsm64's C code
- Python

Cargo fetches everything else.

```
git clone https://github.com/BassGaming/super-sekiro-64
cd super-sekiro-64/libsm64
python import-mario-geo.py
cd ..
.\build.ps1
```

`import-mario-geo.py` (libsm64's own setup script) downloads Mario's model code from the SM64
decompilation once and strips the Nintendo data out of it. That data is read from your ROM when
the game starts.

`build.ps1` builds `sk_mario.dll` and puts it into the `dist` folder, next to `sk-mario.me3` and
`sk_mario.ini`. It swaps the DLL in by renaming the old one, so it also works while the game is
running; the next start loads the new one. `build.ps1 -Dist <folder>` installs into another
folder instead. Then put your ROM into `dist` and double-click `dist\sk-mario.me3`.

Linux can build the Windows DLL as well, with
[`cargo-xwin`](https://github.com/rust-cross/cargo-xwin) (`cargo install --locked cargo-xwin`),
`python3`, `clang` and `llvm`. Use `build.sh` instead of `build.ps1`.

### The title logo

The "Super Sekiro 64" logo art is built into the DLL. The title screen files it goes into are
FromSoftware's, so they aren't shipped. Instead, on the first start, the mod reads them from your
game's `Data1.bdt` and writes patched copies to the `sk_mario_title` folder next to the DLL. Your
install isn't changed. `sk-mario.me3` loads that folder as a package. When the logo changes in an
update, the mod rebuilds it on its own. `title = off` in `sk_mario.ini` skips the logo.

The logo is drawn by `tools/title/logo.py`, and `tools/title/export_logo.py` turns it into the
texture the DLL embeds (`src/assets/title_logo.dds`); both need Python and Pillow.
`tools/title/build_title.py` is the same patcher in Python, for building the package by hand.

## Controls

| Mario mode | Pad | Keyboard |
| --- | --- | --- |
| Move | Left stick | WASD |
| Jump | A | Space |
| Punch, kick (deathblows too) | X | N |
| Crouch, ground pound (in the air) | LT | K |
| Grappling hook | B | F |
| Action (talk, pick up, doors) | Y | E |
| Deflect | LB | |
| Camera | Right stick | Mouse |

| Key | |
| --- | --- |
| F1 | the controls and keys on screen |
| Shift+F2 / Shift+F3 | Mario's damage multiplier lower / higher (0.1 steps) |
| F6 | boss grabs on / off |
| F7 | SM64 music / Sekiro's music |
| F8 (pad: Select) | Mario / plain Sekiro |
| F9 (pad: L3) | Lakitu's camera / Sekiro's |
| F10 | back to where Mario stood 10 s ago (again: further) |
| F11 | log the collision around Mario (for invisible walls) |

## Settings

`sk_mario.ini` next to the DLL holds the settings, each explained in the file. Among them:
- **Combat:**
  - `damage`: the damage multiplier
  - `boss_hit_chance` and `boss_hit_blocks`: how often hits get through a boss's guard
  - `boss_hit_punch`, `boss_hit_dive`, `boss_hit_pound` and `boss_hit_throw`: what those hits
    take, in % of the boss's max HP
- **Boss grabs:**
  - `grab_chance`: how often a punch from behind grabs a boss (0.2)
  - `throw_posture`: the posture damage of a throw (20%)
  - `throw_return_delay`: how long a boss thrown out of reach takes to come back (3 s)
- **Camera:** `camera = sekiro` starts with Sekiro's camera instead of Lakitu's.

## Reporting problems

Open an [issue](https://github.com/BassGaming/super-sekiro-64/issues) and say what happened and when.
Attach `logs/sk_mario.log` (the last session) and `logs/sk_mario.prev.log` (the one before) from
the mod folder. The logs contain the mod folder's path, which can include your Windows user name;
feel free to blank it out.

## License

[MIT](LICENSE), like ER Mario. This covers the mod's own code only, not Super Mario 64's or
Sekiro's content. Mario's model, textures, sounds and animations come from the player's ROM, and
nothing of FromSoftware's is included. The `libsm64` folder keeps libsm64's own license, CC0
([libsm64/LICENSE.md](libsm64/LICENSE.md)).

## Credits

- [ER Mario](https://github.com/deltarooo/er-mario) by deltarooo and its contributors, which this
  mod is forked from
- [libsm64](https://github.com/libsm64/libsm64) (CC0) by the libsm64 contributors, built on the
  [Super Mario 64 decompilation](https://github.com/n64decomp/sm64) by the n64decomp team
- [fromsoftware-rs](https://github.com/vswarte/fromsoftware-rs) (MIT / Apache-2.0) by Vincent
  Swarte and contributors
- [me3](https://me3.help) by the me3 team
- [hudhook](https://github.com/veeenu/hudhook) (MIT) by veeenu, for the overlay HUD
- Data1.bdt's archive key from [UXM](https://github.com/JKAnderson/UXM) by JKAnderson

Super Mario 64 and Mario are Nintendo's. Sekiro is FromSoftware's. This is a free fan mod, not
affiliated with either.

If Fromsoftware or Nintendo want me to remove this repo, just ask. Like I can fight any of you.
