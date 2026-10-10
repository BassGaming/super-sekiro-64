# Rock and moving-platform collision

`collision_geometry.rs` verifies the production winding conversion after X reflection:
floors stay floors, ceilings stay ceilings, rock faces keep their exterior, inconsistent convex
face indices become outward, and quantized degenerate triangles are dropped.

`collision_platforms.c` links the production surface loader, collision queries, platform
displacement and matrix routines. It checks a leaning rock wall and its triangle seam across
static refreshes; static-to-object floor ties and rider attachment; the first lift movement;
dynamic-only floor continuity after removing all static surfaces;
100 rising/descending steps with displacements exceeding the floor query's 78-unit buffer;
stopping, translation plus rotation, jumping away, platform deletion and slot reuse.
The workflow also runs the Rust reflected-platform rotation test through `cargo test`.
All fixtures are synthetic and require no game assets, ROM or running game.

The Windows workflow compiles and executes these tests before building the release DLL.

## Adjacent wooden stakes (combined playtest report, 2026-10-02)

The user became trapped among wooden stakes after a regular jump; an enemy hit freed Mario.
The screenshot does not establish which live triangles were involved. A production collision
query regression reproduces a relevant defect: both triangles on a shared edge use the original
probe position, applying the same wall push twice and crossing the opposite obstacle.
Each face now uses the position corrected by the preceding face. The synthetic 70-unit gap
checks both entry sides, all 24 face orders, repeated collision queries and surface refreshes.
The test fails before the change and passes afterward; the earlier rock/elevator checks also
pass. This is a prevention change to the wall solver, not an automatic unstuck adjustment.
Replaying the reported regular jump in-game remains necessary to confirm this incident is fixed.

## Fog gates

The movement guard that checked Mario's movement against the game's player blockers was taken
out again: the game's rays still hit blockers it had already removed (boss fog walls after the
fight, opened doors), which left invisible walls. Fog gates are back to how they were before.

## Invisible collision after descending the Liurnia cliff

The user reported blocked walking and jumping in southern Liurnia after jumping from the
post-Godrick cliff and landing with Hover. The screenshot alone cannot identify the surface.
The production ceiling query had a relevant defect: `vec3f_find_ceil` anchored its search to
the next floor even when it was far below Mario. A synthetic cliff fixture reproduces a
downward-facing surface below Mario becoming an overhead obstruction; it fails before the
fix. The query now starts at the greater of Mario's feet and the candidate floor height.
Tests also preserve a genuine low roof and ceiling checks above raised landing floors.
All callers of the existing ceiling helper use this correction. Re-test the reported ledge
and check real roofs/overhangs in-game before claiming the live incident is resolved.

## Boss throw through arena walls

BOFA remained stuck after being thrown through a solid wall, but returned after a fog-wall
throw. Carry placement now checks the boss's leading radius against the map. Guided flight
no longer ignores detected contacts for its first 0.08 seconds. Ragdoll crossings remember the
near-side contact and restore the root there after the ragdoll settles, before the boss AI
resumes. The existing rule against disabling a tumbling ragdoll is preserved. Throw rays check
the player-blocker filter as well as
the existing terrain filter. The off-map return behavior remains in place.

Portable tests cover radius separation, reverse-direction contacts and very close hits without
backward teleportation. Live validation must cover carrying/spinning near walls, immediate
throws into walls, full-speed/ragdoll impacts, ground impacts and fog-boundary throws. Confirm
impact damage is applied once and the surviving boss resumes the fight on the reachable side.

## Reported live results and upstream port

Alaric confirmed the stakes, stair-railing overhang, southern Liurnia invisible barrier,
fog-wall blocking and normal Interact entry, and BOFA boss-throw fixes in the combined fork
playtest (final confirmations on `6d88a08`). Rocks and the previously problematic elevator
also passed earlier. This upstream port contains only the collision/throw corrections and
their tests, based on upstream `4308bac`; it preserves upstream's subsequent floor/ceiling
orientation correction and other 0.3.4 changes. The exact port has not been replayed in-game.

## In-game validation still required

The reported rock location was unspecified. No Elden Ring session was launched during this
change, so test success does not establish coverage of every live Havok mesh/layer or the
original clipping incident. The draft DLL should be checked on several stationary boulders
from ground and air, along seams and sloping faces, and on an elevator from rest in both
directions. Check stepping onto/off the lift, jumping while it moves, landing on it, stopping
at both endpoints, and travelling far enough to trigger world-origin rebasing or streaming.

Mesh triangles now retain Havok winding after the coordinate reflection rather than changing
with Mario's location. Convex shapes and synthetic custom-piece boxes explicitly face outward.
Havok accepts two-sided mesh hits; libsm64 uses oriented surfaces, so meshes with intentionally
inconsistent or reversed authoring need particular attention in the in-game check.

This upstream contribution starts at `bf558b37449a7a1a73e870a0ec89c3fd8785f574`
and contains only collision changes and their regression coverage. Synthetic tests and a
Windows build do not replace the live-game checks above; keep the PR in draft until those pass.
