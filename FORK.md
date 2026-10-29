# tjcrone/solidcraft: an unofficial fork of SolidCraft

This is Tim Crone's personal fork of [SolidCraft](https://github.com/tangency-labs/solidcraft). It is not an official SolidCraft release, and it is not affiliated with or endorsed by SolidCraft's maintainers. Everything here builds on upstream's `main` and keeps upstream's licence: MIT OR Apache-2.0 (LICENSE-MIT, LICENSE-APACHE, NOTICE).

## Branches

Two branches stack on upstream's `main`, and are rebased onto it as it moves, so their history is rewritten and force-pushed.

| Branch | Built on | What it adds |
|---|---|---|
| `print-workspace` | upstream `main` | A Print workspace for FFF 3D printing: slicing with sx-core, the SlicerX engine (Apache-2.0), driven by OrcaSlicer-style printer, filament and process files; per-design print settings; a layer-by-layer bead preview; upload to a Klipper printer through Moonraker, with its camera, temperatures, pause, resume, cancel and timelapse switch. |
| `larger-ribbon-icons` | `print-workspace` | A "Larger icons" option for the ribbon without labels: taller buttons with bigger icons, the command names in their tooltips. |

`larger-ribbon-icons` is the default branch and holds both. Slicing for 3D printing is made possible by SlicerX: https://slicerx.app/support.

## Building

The same as upstream (CONTRIBUTING.md). The macOS app: `packaging/macos/package.sh --arch aarch64` for a DMG, or `cargo build --release -p solidcraft` for the program alone.

## Following upstream

New upstream commits are reviewed before they are built. Then each branch is rebased in order (`print-workspace` onto `upstream/main`, `larger-ribbon-icons` onto `print-workspace`) and pushed with `--force-with-lease`. Another clone picks up the rebased branches with `git fetch origin && git reset --hard origin/<branch>`, which discards anything there that was not pushed.

## Remotes

| Remote | Repository | Used for |
|---|---|---|
| `upstream` | `https://github.com/tangency-labs/solidcraft.git` | fetching upstream's `main`; never pushed to (its push URL is `DISABLED`) |
| `origin` | `git@github.com:tjcrone/solidcraft.git` | these branches |
