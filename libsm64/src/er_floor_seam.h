#ifndef ER_FLOOR_SEAM_H
#define ER_FLOOR_SEAM_H
struct MarioState;
/* A tiny default-terrain bevel supported on both sides by near-level floor. */
int er_floor_seam_has_flat_support(struct MarioState *m);
#endif
