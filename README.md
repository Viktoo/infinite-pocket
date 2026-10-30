![Infinite Pocket: 3D letters in an autumn meadow, an orchardist with a basket of apples beside them and a white cat walking past](logo/infinite-pocket-banner.jpg)

# Infinite Pocket

**Infinite Pocket is a creative game powered by an AI harness for creating and interacting with 3D digital worlds.** 

![A slow flight down over Maple Cove's village green at golden hour: a gazebo, pumpkins and flower stalls, homes all round and the sea behind](docs/clips/hero-flyover.webp)

Type what you want to create or do and it happens: `/create a hobbit hut`, `/conjure a pegasus I can ride`, `/poke my eye out with this stick`. The AI writes it as code, on the spot, and it becomes part of the world.

The people in it remember you. They have homes, needs, friends and plans. They gossip, collaborate, play catch, and make things of their own. Dogs fetch, sheep bolt from wolves, fire spreads. At night something comes for you.

![Pine Hollow in the morning: a father plays catch with his son by the campfire while neighbours gather and a dog wanders past](docs/clips/campfire-catch.webp)

Then invite a friend into your world and build it together.

## Install (Mac with Apple Silicon)

Needs [Homebrew](https://brew.sh).

```bash
brew install viktoo/tap/pocket
```

```bash
pocket
```

Then add an Anthropic API key in **Settings → AI**. Get one at [platform.claude.com](https://platform.claude.com/settings/keys).

Characters' voices (354 MB) and speak to talk (148 MB) are optional downloads in **Settings → Sound**.

New versions come out often. To update:

```bash
brew upgrade pocket
```

To play together, everyone needs the same version.

Windows is coming.

## Play


|                        |                                                  |
| ---------------------- | ------------------------------------------------ |
| `W` `A` `S` `D`, mouse | walk and look (click the view to grab the mouse) |
| `Enter`                | talk to whoever is in front of you               |
| `/`                    | make or do anything, in words                    |
| `e` `g` `f`            | use, pick up, throw                              |
| `Esc`                  | pause, settings, play together                   |

![Playing: holding a sword in Lilyshore Meadows while Aino Lahti warns you off the nets, with the keys for what you point at, the feed, and the bar to type into](docs/img/playing.webp)

## Multiplayer

Host a world and your friends join it, on the same network or over the internet with ngrok. See [Multiplayer](docs/multiplayer.md).

## Build worlds with a coding agent

Everything in the game is code the AI can read and write, and the game can check its own work. `pocket` takes screenshots, records clips, describes what's in view, and runs a world headless, so a coding agent (Claude Code, Codex, Cursor…) can build a place, look at it, and fix it.

An agent can look at anyone it made, in front of their own home. Here are 36 people and creatures from 13 generated worlds (`pocket snapshot my.pocket --face ID --open`):

![36 portraits of people and creatures from 13 generated worlds, each in front of their own home](docs/clips/faces-sheet.jpg)

## Docs

- [Full guide](docs/guide.md): every command, setting and how it all works
- [Playing together](docs/multiplayer.md)
- [Species, bodies and lives](docs/species.md)
- [Bodies, things and tools](docs/physics.md)
- [The land](docs/terrain.md), [ways, water and hollows](docs/ways.md), [under the ground and up off it](docs/levels.md)
- [Weather, clouds and sunsets](docs/weather.md)
- [Sound](docs/sound.md), [voices and speaking to talk](docs/voices.md)
- [Releases](docs/releases.md)
- [Contributing](CONTRIBUTING.md)

## Build from source

The `--tag` builds the release brew installs, so you can play with Mac players. Leave it off for the latest code.

### Mac

```bash
xcode-select --install
```

```bash
brew install cmake
```

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Then open a new terminal, so `cargo` is found.

```bash
cargo install --locked --git https://github.com/Viktoo/infinite-pocket --tag v0.3.0
```

### Windows

In PowerShell:

```powershell
winget install Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

```powershell
winget install Rustlang.Rustup Kitware.CMake LLVM.LLVM Git.Git
```

Then open a new PowerShell, so `cargo` is found.

```powershell
cargo install --locked --git https://github.com/Viktoo/infinite-pocket --tag v0.3.0
```

Then run `pocket`. To host, allow it on private networks when Windows Firewall asks. To update, run the `cargo install` line again with the new release's tag.

Tests and releases: [docs/guide.md](docs/guide.md#tests), [docs/releases.md](docs/releases.md).

## License

[GPL-3.0](LICENSE)
