#include "er_floor_seam.h"
#include "decomp/include/sm64.h"
#include "decomp/game/mario.h"
#include "decomp/engine/surface_collision.h"
#include <math.h>

int er_floor_seam_has_flat_support(struct MarioState *m) {
    if (!m->floor || m->curTerrain == TERRAIN_SLIDE
        || mario_get_floor_class(m) != SURFACE_CLASS_DEFAULT
        || m->floor->normal.y > 0.7880108f
        || (m->action & ACT_FLAG_AIR)
        || fabsf(m->pos[1] - m->floorHeight) > 5) return 0;
    float nx = m->floor->normal.x, nz = m->floor->normal.z;
    float length = sqrtf(nx * nx + nz * nz);
    if (!isfinite(length) || length < 0.001f) return 0;
    /* Probe across the steep triangle, not along its seam. find_floor allows
       floors up to 78 units above the query, so validate each returned height
       explicitly: a bridge/step above or a floor below cannot count as support. */
    for (int sign = -1; sign <= 1; sign += 2) {
        struct SM64SurfaceCollisionData *floor = NULL;
        float h = find_floor(m->pos[0] + sign * 20 * nx / length,
                             m->floorHeight, m->pos[2] + sign * 20 * nz / length, &floor);
        if (!floor || floor->transform != m->floor->transform
            || !isfinite(h) || fabsf(h - m->floorHeight) > 8
            || floor->normal.y < 0.9659258f) return 0;
        struct MarioState sample = *m;
        sample.floor = floor;
        sample.curTerrain = floor->terrain;
        if (mario_get_floor_class(&sample) != SURFACE_CLASS_DEFAULT) return 0;
    }
    return 1;
}
