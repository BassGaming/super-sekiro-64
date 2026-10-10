/* Exercise the production surface loader, floor/wall queries, matrices and rider displacement.
 * No game, ROM, Mario model or private assets are needed. */
#include <assert.h>
#include <math.h>
#include <stdio.h>
#include "load_surfaces.h"
#include "debug_print.h"
#include "decomp/global_state.h"
#include "decomp/shim.h"
#include "decomp/include/object_fields.h"
#include "decomp/include/sm64.h"
#include "decomp/engine/surface_collision.h"
#include "decomp/game/platform_displacement.h"

SM64DebugPrintFunctionPtr g_debug_print_func = NULL;
static struct GlobalState state;
struct GlobalState *g_state = &state;
static struct Object mario;

static const struct SM64Surface deck[] = {
    {0, 0, 0, {{-500, 0, -500}, {500, 0, 500}, {500, 0, -500}}},
    {0, 0, 0, {{-500, 0, -500}, {-500, 0, 500}, {500, 0, 500}}}
};

static void sync_and_check_floor(void) {
    struct MarioState *m = &state.mgMarioStateVal;
    struct SM64SurfaceCollisionData *floor;
    float height = find_floor(m->pos[0], m->pos[1], m->pos[2], &floor);
    assert(floor != NULL && floor->transform == mario.platform);
    assert(fabsf(height - m->pos[1]) < 2.0f);
    m->floor = floor; m->floorHeight = height;
    mario.oPosX = m->pos[0]; mario.oPosY = m->pos[1]; mario.oPosZ = m->pos[2];
    update_mario_platform();
    assert(mario.platform != NULL);
}

static void lifts(void) {
    state.mgMarioObject = &mario;
    struct MarioState *m = &state.mgMarioStateVal;
    m->action = ACT_IDLE; m->pos[0] = 100; m->pos[1] = 0; m->pos[2] = 0;
    surfaces_load_static(deck, 2);
    struct SM64SurfaceObject object = {{{0, 0, 0}, {0, 0, 0}}, 2, (struct SM64Surface *)deck};
    uint32_t id = surfaces_load_object(&object);
    struct SM64SurfaceObjectTransform *platform = surfaces_object_get_transform_ptr(id);
    er_attach_platform(&state, platform);
    assert(mario.platform == platform); /* static copy must not win a floor tie */
    surfaces_load_static(NULL, 0); /* same-tick promotion removes the stale floor */

    /* First motion and each subsequent rising/descending tick, including >78-unit deltas. */
    for (int tick = 0; tick < 100; tick++) {
        float dy = tick < 50 ? 100.0f : -100.0f;
        object.transform.position[1] += dy;
        surface_object_update_transform(id, &object.transform);
        apply_mario_platform_displacement();
        assert(fabsf(m->pos[1] - object.transform.position[1]) < 0.01f);
        sync_and_check_floor();
    }
    /* Stopping does not replay the previous delta. */
    surface_object_update_transform(id, &object.transform);
    apply_mario_platform_displacement();
    assert(fabsf(m->pos[1]) < 0.01f);
    sync_and_check_floor();

    object.transform.position[0] = 20; object.transform.position[2] = 30;
    object.transform.eulerRotation[1] = -90;
    surface_object_update_transform(id, &object.transform);
    apply_mario_platform_displacement();
    assert(fabsf(m->pos[0] - 20) < 0.1f && fabsf(m->pos[2] + 70) < 0.1f);
    sync_and_check_floor();

    object.transform.eulerRotation[0] = -15;
    object.transform.eulerRotation[1] = -20;
    object.transform.eulerRotation[2] = -10;
    surface_object_update_transform(id, &object.transform);
    apply_mario_platform_displacement();
    sync_and_check_floor(); /* pitch/roll must move both the rider and its floor */

    m->pos[1] += 200; mario.oPosY = m->pos[1];
    m->action = ACT_FREEFALL;
    update_mario_platform();
    assert(mario.platform == NULL); /* jumping/falling never drags a rider */
    er_attach_platform(&state, platform);
    assert(mario.platform == NULL);

    /* Disappearing/streamed platforms cannot leave dangling floor/wall/ceiling references. */
    m->wall = m->ceil = m->floor;
    mario.platform = platform;
    er_detach_platform(&state, platform);
    assert(mario.platform == NULL && m->floor == NULL && m->wall == NULL && m->ceil == NULL);
    surfaces_unload_object(id);
    apply_mario_platform_displacement();
    assert(surfaces_object_get_transform_ptr(id) == NULL);
    id = surfaces_load_object(&object);
    assert(surfaces_object_get_transform_ptr(id)->aVelY == 0); /* reused slot starts clean */
    surfaces_unload_all();
}

static void rock_wall(void) {
    const struct SM64Surface rock[] = {
        {0, 0, 0, {{0, 0, -200}, {20, 200, 200}, {0, 0, 200}}},
        {0, 0, 0, {{0, 0, -200}, {20, 200, -200}, {20, 200, 200}}}
    };
    /* Stable outward winding must push a slightly penetrated player out, including the seam
     * between the two triangles. Refreshing the static list must give the same result. */
    for (int scan = 0; scan < 5; scan++) {
        surfaces_load_static(rock, 2);
        for (int z = -100; z <= 100; z += 20) {
            float x = -5, y = 100, zz = z;
            assert(f32_find_wall_collision(&x, &y, &zz, 0, 50) > 0);
            assert(x >= 59.0f);
        }
    }
    surfaces_unload_all();
}

static void stake_gap(void) {
    /* A jump's 35-unit wall probe lands on the shared edge of two triangles.
     * Both left faces must not apply the same displacement from the original
     * position: that pushes Mario through the opposite stake at x=70. */
    const struct SM64Surface stakes[] = {
        {0, 0, 0, {{0, 0, -200}, {0, 200, 200}, {0, 0, 200}}},
        {0, 0, 0, {{0, 0, -200}, {0, 200, -200}, {0, 200, 200}}},
        {0, 0, 0, {{70, 0, -200}, {70, 0, 200}, {70, 200, 200}}},
        {0, 0, 0, {{70, 0, -200}, {70, 200, 200}, {70, 200, -200}}}
    };
    /* Havok refreshes need not enumerate adjacent faces in the same order. */
    for (int a = 0; a < 4; a++)
    for (int b = 0; b < 4; b++)
    for (int c = 0; c < 4; c++)
    for (int d = 0; d < 4; d++) {
        if (a == b || a == c || a == d || b == c || b == d || c == d) continue;
        const struct SM64Surface ordered[] = {stakes[a], stakes[b], stakes[c], stakes[d]};
        for (int refresh = 0; refresh < 5; refresh++) {
            surfaces_load_static(ordered, 4);
            for (int side = 0; side < 2; side++) {
                float x = side ? 75 : -5, y = 100, z = 0;
                assert(f32_find_wall_collision(&x, &y, &z, 0, 35) > 0);
                assert(fabsf(x - 35) < 0.01f);
                assert(fabsf(z) < 0.01f && y == 100);
                for (int tick = 0; tick < 10; tick++) {
                    f32_find_wall_collision(&x, &y, &z, 0, 35);
                    assert(fabsf(x - 35) < 0.01f);
                }
            }
        }
    }
    surfaces_unload_all();
}

static void ledge_ceiling(void) {
    const struct SM64Surface valley[] = {
        {0, 0, 0, {{-500, -1000, -500}, {500, -1000, 500}, {500, -1000, -500}}},
        {0, 0, 0, {{-500, -200, -500}, {500, -200, -500}, {500, -200, 500}}}
    };
    surfaces_load_static(valley, 2);
    struct SM64SurfaceCollisionData *floor, *ceil;
    Vec3f pos = {0, 0, 0};
    float floorHeight = find_floor(pos[0], pos[1], pos[2], &floor);
    assert(floor && floorHeight == -1000);
    /* The downward face two metres BELOW Mario is not an overhead obstacle,
     * even though the next floor is ten metres down across the ledge. */
    find_ceil_above_mario(pos[0], pos[1], pos[2], floorHeight, &ceil);
    assert(ceil == NULL);
    /* A real roof still blocks the jump, including a low head-bonk roof. */
    const struct SM64Surface roof[] = {
        {0, 0, 0, {{-500, 100, -500}, {500, 100, -500}, {500, 100, 500}}}
    };
    surfaces_load_static(roof, 1);
    assert(find_ceil_above_mario(0, 0, 0, -1000, &ceil) == 100 && ceil);
    /* A raised landing floor, rather than the old feet height, is still used. */
    assert(find_ceil_above_mario(0, -100, 0, 0, &ceil) == 100 && ceil);
    surfaces_unload_all();
}

int main(void) {
    ledge_ceiling();
    stake_gap();
    rock_wall();
    lifts();
    puts("ledge ceilings, stake seams, rock collision and elevator regression tests passed");
    return 0;
}
