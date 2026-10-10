#ifndef PLATFORM_DISPLACEMENT_H
#define PLATFORM_DISPLACEMENT_H

#include "../include/PR/ultratypes.h"
#include "../include/types.h"

struct GlobalState;
struct SM64SurfaceObjectTransform;
void er_attach_platform(struct GlobalState *state, struct SM64SurfaceObjectTransform *platform);
void er_detach_platform(struct GlobalState *state, struct SM64SurfaceObjectTransform *platform);
void update_mario_platform(void);
void apply_mario_platform_displacement(void);

#endif // PLATFORM_DISPLACEMENT_H
