# fabric-mods: notes for agents

You are in the Fabric mod ports of Horizon Fabric. Read `../docs/fabric-development.md` (how this repository is
developed) and `README.md` here (layout, installing) first; the skill `.claude/skills/fabric-port` walks a port.

- One folder per mod: `<mod-id>/README.md` plus `content/` (horizon-mod.toml) and/or `native/` (horizon-mixin.toml).
  `python3 tools/check_fabric_mods.py` from the repository root checks the layout.
- The original's licence must allow the port; name the original, its link and licence in the README.
- No jars, no built `.hmod`/`.hnmod`, no Mojang or copied assets. Built files go to Releases
  (`<mod-id>-v<version>`).
- Native mods: no network, no files outside the game folder, no processes. Explain each hook in the pull request.
- A hook the port needs and Horizon lacks goes into the engine first, in its own pull request
  (`docs/fabric-api.md` lists the gaps and the smallest hook for each).
- Test in game with scratch `HORIZON_HOME`/`XDG_*` dirs and say what you checked.
