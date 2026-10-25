# Maple Cove, the starter world

Maple Cove (`worlds/starter`, see its README) ships inside the game. Made
once by us with `pocket author`; free for every player after, with no
network and no AI needed to see it or walk in it.

## Making it

```bash
python3 worlds/starter/make.py
```

```bash
./target/release/pocket author worlds/starter
```

```bash
gzip -9 -kf worlds/starter/starter.pocket
```

`starter.pocket.gz` (about 0.2 MB) is committed and built into the game
(`src/starter.rs`). The file keeps a fingerprint of the folder it was made
from (kv `authored.source`); a test fails when the folder changed and the
file wasn't made again. `world.json` may set `start_hour` and `facing`
(degrees): the time and way a traveler first arrives (Maple Cove: 5:30 am, dawn,
facing the gazebo).

## Its lifecycle

- **Behind the title and the loading screen**: a throwaway copy,
  `~/.pocket/backdrop/maple-cove-<version>.pocket`, written again whenever
  the game ships a newer Maple Cove (older ones are removed). Never saved
  to, no AI. The views are `worlds/starter/shots.json`; it opens on the
  cove at sunset. It runs from the title into the next world's loading
  screen and is let go once that world is stepped into; it starts again on
  the way back to the title.
- **Your own Maple Cove**: with no worlds yet, the title's first button is
  **Open Maple Cove**. It copies the town into your worlds
  (`universes/maple-cove.pocket`, kv `starter.version`) and opens it. From
  then on it is a world like any other: Continue opens it, it is listed in
  Your worlds, and past its edges it is made further as you walk (which
  costs tokens), like any world. A newer Maple Cove never touches it.
  Delete every world and Open Maple Cove is offered again.
- **Play together**: hosting offers the most recent world; with none yet,
  Maple Cove (copied when hosted).

## Not done

- A "newer Maple Cove is out" note for players with an old copy
  (`starter.version` is kept for it).
- Adopting edits from a played copy back into the folder
  (`pocket author adopt COPY DIR`).
