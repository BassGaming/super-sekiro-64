/* Production surface loader/queries + seam guard. No ROM or game required.
   Floor class seam is an explicit stub; flat/default and tagged non-default
   cases are covered here, and the extraction probe uses the real classifier. */
#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <string.h>
#include "er_floor_seam.h"
#include "load_surfaces.h"
#include "debug_print.h"
#include "decomp/global_state.h"
#include "decomp/include/sm64.h"
#include "decomp/engine/surface_collision.h"

SM64DebugPrintFunctionPtr g_debug_print_func = NULL;
static struct GlobalState state;
struct GlobalState *g_state = &state;
s32 mario_get_floor_class(struct MarioState *m) {
    if(m->curTerrain==TERRAIN_SLIDE)return SURFACE_CLASS_VERY_SLIPPERY;
    switch(m->floor->type) {
        case SURFACE_SLIPPERY:return SURFACE_CLASS_SLIPPERY;
        case SURFACE_VERY_SLIPPERY:return SURFACE_CLASS_VERY_SLIPPERY;
        case SURFACE_NOT_SLIPPERY:return SURFACE_CLASS_NOT_SLIPPERY;
        default:return SURFACE_CLASS_DEFAULT;
    }
}
static struct SM64Surface triangles[6];
static void quad(int at,int x0,int x1,int y0,int y1) {
    int p[4][3]={{x0,y0,-100},{x1,y1,-100},{x1,y1,100},{x0,y0,100}};
    int order[2][3]={{0,2,1},{0,3,2}};
    for(int t=0;t<2;t++)for(int v=0;v<3;v++)for(int k=0;k<3;k++)
        triangles[at+t].vertices[v][k]=p[order[t][v]][k];
}
static struct MarioState scene(int halfwidth,int rise,int left,int right) {
    memset(triangles,0,sizeof triangles);
    quad(0,-halfwidth,halfwidth,-rise,rise);
    quad(2,-100,-halfwidth,left,left);
    quad(4,halfwidth,100,right,right);
    surfaces_load_static(triangles,6);
    struct MarioState m={0};m.action=ACT_WALKING;m.pos[0]=m.pos[2]=0;m.pos[1]=0;
    m.floorHeight=find_floor(0,0,0,&m.floor);
    assert(m.floor&&fabsf(m.floorHeight)<0.001f);
    return m;
}
int main(void) {
    struct MarioState m=scene(3,3,-3,3),before=m;
    assert(m.floor->normal.y<0.7880108f);
    assert(er_floor_seam_has_flat_support(&m));
    assert(!memcmp(&m,&before,sizeof m)); /* Query does not replace Mario's floor. */
    printf("6cm bevel: old auto-slide=1 guarded auto-slide=0\n");
    m=scene(60,60,-60,60);assert(!er_floor_seam_has_flat_support(&m)); /* Broad45degree ramp. */
    m=scene(3,3,-3,16);assert(!er_floor_seam_has_flat_support(&m)); /* High step. */
    m=scene(3,3,-16,3);assert(!er_floor_seam_has_flat_support(&m)); /* Low ledge. */
    m=scene(3,3,-3,3);surfaces_load_static(triangles,4);
    m.floorHeight=find_floor(0,0,0,&m.floor);
    assert(!er_floor_seam_has_flat_support(&m)); /* Missing support on one side. */
    m=scene(3,3,-3,3);
    struct SM64SurfaceObject support={{{0,0,0},{0,0,0}},2,&triangles[4]};
    uint32_t object=surfaces_load_object(&support);
    assert(!er_floor_seam_has_flat_support(&m)); /* Different moving owner. */
    surfaces_unload_object(object);
    m=scene(3,3,-3,3);m.curTerrain=TERRAIN_SLIDE;assert(!er_floor_seam_has_flat_support(&m));
    m=scene(3,3,-3,3);m.floor->type=SURFACE_SLIPPERY;assert(!er_floor_seam_has_flat_support(&m));
    m=scene(3,3,-3,3);m.floor->type=SURFACE_VERY_SLIPPERY;assert(!er_floor_seam_has_flat_support(&m));
    m=scene(3,3,-3,3);triangles[4].type=triangles[5].type=SURFACE_SLIPPERY;
    surfaces_load_static(triangles,6);m.floorHeight=find_floor(0,0,0,&m.floor);
    assert(!er_floor_seam_has_flat_support(&m)); /* Slippery adjacent support. */
    m=scene(3,3,-3,3);triangles[4].terrain=triangles[5].terrain=TERRAIN_SLIDE;
    surfaces_load_static(triangles,6);m.floorHeight=find_floor(0,0,0,&m.floor);
    assert(!er_floor_seam_has_flat_support(&m)); /* Slide terrain adjacent support. */
    m=scene(3,3,-3,3);m.action=ACT_FREEFALL;assert(!er_floor_seam_has_flat_support(&m));
    m=scene(3,3,-3,3);m.pos[1]=6;assert(!er_floor_seam_has_flat_support(&m));
    m=scene(3,3,-3,3);m.floor=NULL;assert(!er_floor_seam_has_flat_support(&m));
    /* Intentional flat-floor slides retain their action/velocity; this helper
       only controls automatic INPUT_ABOVE_SLIDE initialization in mario.c. */
    quad(0,-100,100,0,0);surfaces_load_static(triangles,2);
    memset(&m,0,sizeof m);m.action=ACT_BUTT_SLIDE;m.forwardVel=32;
    m.floorHeight=find_floor(0,0,0,&m.floor);before=m;
    assert(!er_floor_seam_has_flat_support(&m)&&!memcmp(&m,&before,sizeof m));
    puts("broad slope/gap/height/terrain/air fail-open; intentional slide unchanged");
    surfaces_unload_all();return 0;
}
