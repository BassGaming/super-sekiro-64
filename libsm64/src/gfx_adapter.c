#include <stdlib.h>
#include <string.h>

#include "libsm64.h"
#include "decomp/engine/math_util.h"
#include "decomp/engine/guMtxF2L.h"
#include "gfx_adapter.h"
#include "gfx_adapter_commands.h"
#include "load_tex_data.h"

static Mat4 s_curMatrix;
static float s_curColor[3];

static uint16_t s_scaleS, s_scaleT, s_uls, s_ult;
static int s_textureOn, s_textureIndex;
static float s_texWidth;
static float s_texHeight;

static struct SM64MarioGeometryBuffers *s_outBuffers;

static float *s_trianglePtr;
static float *s_colorPtr;
static float *s_normalPtr;
static float *s_uvPtr;

// er-mario patch: per-part matrices and per-triangle part ids + local (untransformed) geometry,
// so the model can be rebuilt as a rigidly skinned engine mesh and posed from these matrices.
#define ER_MAX_PARTS 64
float er_part_matrices[ER_MAX_PARTS][16];
int er_part_count;
int er_tri_part[SM64_GEO_MAX_TRIANGLES];
float er_local_pos[SM64_GEO_MAX_TRIANGLES * 9];
float er_local_normal[SM64_GEO_MAX_TRIANGLES * 9];

static void mtxf_mul_vec3f_x(Mat4 mtx, Vec3f b, float w, Vec3f out)
{
    out[0] = b[0] * mtx[0][0] + b[1] * mtx[1][0] + b[2] * mtx[2][0] + w * mtx[3][0];
    out[1] = b[0] * mtx[0][1] + b[1] * mtx[1][1] + b[2] * mtx[2][1] + w * mtx[3][1];
    out[2] = b[0] * mtx[0][2] + b[1] * mtx[1][2] + b[2] * mtx[2][2] + w * mtx[3][2];
}

static void convert_uv_to_atlas( float *atlas_uv_out, short tc[] )
{
    float u = (float)((tc[0] * s_scaleS >> 16) - 8*s_uls) / 32.0f / s_texWidth;
    float v = (float)((tc[1] * s_scaleT >> 16) - 8*s_ult) / 32.0f / s_texHeight;

    atlas_uv_out[0] = u * s_texWidth / 64.0f / (float)NUM_USED_TEXTURES + (float)s_textureIndex / (float)NUM_USED_TEXTURES;
    atlas_uv_out[1] = v * s_texHeight / 64.0f;
}

static void process_display_list( void *dl )
{
    intptr_t *ptr = (intptr_t *)dl;
    Vtx *vdata = NULL;

    for( ;; )
    {
        switch( *ptr++ )
        {
            case GFXCMD_VertexData: 
            {
                UNUSED intptr_t v = *ptr++;
                UNUSED intptr_t n = *ptr++;
                UNUSED intptr_t v0 = *ptr++;
                vdata = (Vtx*)v;
                break;
            }

            case GFXCMD_Triangle:
            {
                intptr_t v00 = *ptr++;
                intptr_t v01 = *ptr++;
                intptr_t v02 = *ptr++;
                UNUSED intptr_t flag0 = *ptr++;

                float x0 = vdata[v00].v.ob[0], y0 = vdata[v00].v.ob[1], z0 = vdata[v00].v.ob[2];
                float x1 = vdata[v01].v.ob[0], y1 = vdata[v01].v.ob[1], z1 = vdata[v01].v.ob[2];
                float x2 = vdata[v02].v.ob[0], y2 = vdata[v02].v.ob[1], z2 = vdata[v02].v.ob[2];
                Vec3f p0 = { (float)x0, (float)y0, (float)z0 };
                Vec3f p1 = { (float)x1, (float)y1, (float)z1 };
                Vec3f p2 = { (float)x2, (float)y2, (float)z2 };

                signed char nx0 = vdata[v00].n.n[0], ny0 = vdata[v00].n.n[1], nz0 = vdata[v00].n.n[2];
                signed char nx1 = vdata[v01].n.n[0], ny1 = vdata[v01].n.n[1], nz1 = vdata[v01].n.n[2];
                signed char nx2 = vdata[v02].n.n[0], ny2 = vdata[v02].n.n[1], nz2 = vdata[v02].n.n[2];
                Vec3f n0 = { ((float)nx0) / 128.0f, ((float)ny0) / 128.0f, ((float)nz0) / 128.0f };
                Vec3f n1 = { ((float)nx1) / 128.0f, ((float)ny1) / 128.0f, ((float)nz1) / 128.0f };
                Vec3f n2 = { ((float)nx2) / 128.0f, ((float)ny2) / 128.0f, ((float)nz2) / 128.0f };

                {
                    int tri = (int)((s_trianglePtr - s_outBuffers->position) / 9);
                    if( tri < SM64_GEO_MAX_TRIANGLES )
                    {
                        er_tri_part[tri] = er_part_count - 1;
                        float *lp = &er_local_pos[tri * 9], *ln = &er_local_normal[tri * 9];
                        lp[0] = p0[0]; lp[1] = p0[1]; lp[2] = p0[2]; lp[3] = p1[0]; lp[4] = p1[1]; lp[5] = p1[2];
                        lp[6] = p2[0]; lp[7] = p2[1]; lp[8] = p2[2];
                        ln[0] = n0[0]; ln[1] = n0[1]; ln[2] = n0[2]; ln[3] = n1[0]; ln[4] = n1[1]; ln[5] = n1[2];
                        ln[6] = n2[0]; ln[7] = n2[1]; ln[8] = n2[2];
                    }
                }
                mtxf_mul_vec3f_x( s_curMatrix, p0, 1.0f, s_trianglePtr );
                s_trianglePtr += 3;
                mtxf_mul_vec3f_x( s_curMatrix, p1, 1.0f, s_trianglePtr );
                s_trianglePtr += 3;
                mtxf_mul_vec3f_x( s_curMatrix, p2, 1.0f, s_trianglePtr );
                s_trianglePtr += 3;

                // TODO normals arent correct under non-uniform scale. multiply by inverse/transpose
                mtxf_mul_vec3f_x( s_curMatrix, n0, 0.0f, s_normalPtr );
                vec3f_normalize( s_normalPtr );
                s_normalPtr += 3;
                mtxf_mul_vec3f_x( s_curMatrix, n1, 0.0f, s_normalPtr );
                vec3f_normalize( s_normalPtr );
                s_normalPtr += 3;
                mtxf_mul_vec3f_x( s_curMatrix, n2, 0.0f, s_normalPtr );
                vec3f_normalize( s_normalPtr );
                s_normalPtr += 3;

                *s_colorPtr++ = s_curColor[0];
                *s_colorPtr++ = s_curColor[1];
                *s_colorPtr++ = s_curColor[2];
                *s_colorPtr++ = s_curColor[0];
                *s_colorPtr++ = s_curColor[1];
                *s_colorPtr++ = s_curColor[2];
                *s_colorPtr++ = s_curColor[0];
                *s_colorPtr++ = s_curColor[1];
                *s_colorPtr++ = s_curColor[2];

                if( s_textureOn )
                {
                    convert_uv_to_atlas( s_uvPtr, vdata[v00].v.tc ); s_uvPtr += 2;
                    convert_uv_to_atlas( s_uvPtr, vdata[v01].v.tc ); s_uvPtr += 2;
                    convert_uv_to_atlas( s_uvPtr, vdata[v02].v.tc ); s_uvPtr += 2;
                }
                else
                {
                    *s_uvPtr++ = 1.0f;
                    *s_uvPtr++ = 1.0f;
                    *s_uvPtr++ = 1.0f;
                    *s_uvPtr++ = 1.0f;
                    *s_uvPtr++ = 1.0f;
                    *s_uvPtr++ = 1.0f;
                }

                s_outBuffers->numTrianglesUsed = (uint16_t)((s_trianglePtr - s_outBuffers->position) / 9);

                break;
            }

            case GFXCMD_Light:
            {
                intptr_t l = *ptr++;
                intptr_t n = *ptr++;

                if( n == 1 )
                {
                    Light *data = (Light*)l;
                    s_curColor[0] = (float)data->l.col[0] / 255.0f;
                    s_curColor[1] = (float)data->l.col[1] / 255.0f;
                    s_curColor[2] = (float)data->l.col[2] / 255.0f;
                }
                
                break;
            }

            case GFXCMD_Texture:
            {
                intptr_t s = *ptr++;
                intptr_t t = *ptr++;
                intptr_t on = *ptr++;

                s_scaleS = (uint16_t)s;
                s_scaleT = (uint16_t)t;
                s_textureOn = (int)on;

                break;
            }

            case GFXCMD_SetTextureImage:
            {
                intptr_t i = *ptr++;

                s_textureIndex = (int)i;
                s_texWidth = mario_tex_widths[s_textureIndex];
                s_texHeight = mario_tex_heights[s_textureIndex];

                break;
            }

            case GFXCMD_SetTileSize:
            {
                intptr_t uls = *ptr++;
                intptr_t ult = *ptr++;
                ptr++; // lrs
                ptr++; // lrt

                s_uls = (uint16_t)uls;
                s_ult = (uint16_t)ult;
                
                break;
            }

            case GFXCMD_SubDisplayList:
            {
                intptr_t dl = *ptr++;
                process_display_list( (void*)dl );
                break;
            }

            case GFXCMD_EndDisplayList:
                goto break_top;
        }
    }

break_top:
    {}
}

void gSPMatrix( void *pkt, Mtx *m, uint8_t flags )
{
    guMtxL2F( s_curMatrix, m );
    if( er_part_count < ER_MAX_PARTS )
    {
        for( int r = 0; r < 4; r++ )
            for( int c = 0; c < 4; c++ )
                er_part_matrices[er_part_count][r * 4 + c] = s_curMatrix[r][c];
        er_part_count++;
    }
}

void gSPDisplayList( void *pkt, struct DisplayListNode *dl )
{
    process_display_list( (void*)dl );
}

void gfx_adapter_bind_output_buffers( struct SM64MarioGeometryBuffers *outBuffers )
{
    s_outBuffers = outBuffers;
    s_trianglePtr = s_outBuffers->position;
    s_colorPtr = s_outBuffers->color;
    s_normalPtr = s_outBuffers->normal;
    s_uvPtr = s_outBuffers->uv;
    s_outBuffers->numTrianglesUsed = 0;
    er_part_count = 0;
}