/*
 * FreeRDP cross-check for the chroma analysis (analysis only, not built by
 * cargo).  Links against libfreerdp3 / libwinpr3 (FreeRDP 3.x).
 *
 *   freerdp_check avc-combine <i420 main> <i420 aux> <v1|v2> <w> <h> <out.rgba> [generic|opt]
 *       FreeRDP's YUV420CombineToYUV444 (LUMA then CHROMAv1/v2) followed by
 *       YUV444ToRGB_8u_P3AC4R (which applies FreeRDP's chroma reverse filter
 *       with the threshold of 30).  Output RGBA.
 *   freerdp_check avc-split <in.rgba> <v1|v2> <w> <h> <out_main.i420> <out_aux.i420>
 *       FreeRDP's server-side RGBToAVC444YUV / RGBToAVC444YUVv2.
 *   freerdp_check nsc <stream> <w> <h> <out.rgba>        (FREERDP_FLIP_VERTICAL, SurfaceBits)
 *   freerdp_check clear <stream> <w> <h> <out.rgba>
 *   freerdp_check planar <stream> <w> <h> <out.rgba>
 *   freerdp_check progressive <stream> <w> <h> <out.rgba>   (surface 0, one frame)
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <freerdp/primitives.h>
#include <freerdp/codec/color.h>
#include <freerdp/codec/nsc.h>
#include <freerdp/codec/clear.h>
#include <freerdp/codec/planar.h>
#include <freerdp/codec/progressive.h>
#include <freerdp/codec/region.h>

static BYTE* read_file(const char* path, size_t* len)
{
	FILE* f = fopen(path, "rb");
	if (!f)
	{
		perror(path);
		exit(2);
	}
	fseek(f, 0, SEEK_END);
	*len = (size_t)ftell(f);
	fseek(f, 0, SEEK_SET);
	BYTE* buf = malloc(*len + 1);
	if (fread(buf, 1, *len, f) != *len)
		exit(2);
	fclose(f);
	return buf;
}

static void write_file(const char* path, const BYTE* data, size_t len)
{
	FILE* f = fopen(path, "wb");
	if (!f || fwrite(data, 1, len, f) != len)
	{
		perror(path);
		exit(2);
	}
	fclose(f);
}

static int avc_combine(int argc, char** argv)
{
	if (argc < 8)
		return 1;
	size_t lm = 0, la = 0;
	BYTE* main = read_file(argv[2], &lm);
	BYTE* aux = read_file(argv[3], &la);
	const BOOL v2 = strcmp(argv[4], "v2") == 0;
	const UINT32 w = (UINT32)atoi(argv[5]);
	const UINT32 h = (UINT32)atoi(argv[6]);
	const BOOL generic = (argc > 8) && strcmp(argv[8], "generic") == 0;
	primitives_t* prims = generic ? primitives_get_generic() : primitives_get();

	const BYTE* pMain[3] = { main, main + w * h, main + w * h + (w / 2) * (h / 2) };
	const BYTE* pAux[3] = { aux, aux + w * h, aux + w * h + (w / 2) * (h / 2) };
	const UINT32 srcStep[3] = { w, w / 2, w / 2 };
	BYTE* yuv444 = calloc(3, (size_t)w * h);
	BYTE* pDst[3] = { yuv444, yuv444 + w * h, yuv444 + 2 * w * h };
	const UINT32 dstStep[3] = { w, w, w };
	const RECTANGLE_16 roi = { 0, 0, (UINT16)w, (UINT16)h };

	if (prims->YUV420CombineToYUV444(AVC444_LUMA, pMain, srcStep, w, h, pDst, dstStep, &roi) !=
	    PRIMITIVES_SUCCESS)
		return 3;
	if (prims->YUV420CombineToYUV444(v2 ? AVC444_CHROMAv2 : AVC444_CHROMAv1, pAux, srcStep, w, h,
	                                 pDst, dstStep, &roi) != PRIMITIVES_SUCCESS)
		return 4;

	BYTE* rgba = calloc(4, (size_t)w * h);
	const prim_size_t size = { w, h };
	if (prims->YUV444ToRGB_8u_P3AC4R((const BYTE**)pDst, dstStep, rgba, w * 4, PIXEL_FORMAT_RGBA32,
	                                 &size) != PRIMITIVES_SUCCESS)
		return 5;
	for (size_t i = 0; i < (size_t)w * h; i++)
		rgba[i * 4 + 3] = 0xFF;
	write_file(argv[7], rgba, (size_t)w * h * 4);
	return 0;
}

static int avc_split(int argc, char** argv)
{
	if (argc < 8)
		return 1;
	size_t len = 0;
	BYTE* rgba = read_file(argv[2], &len);
	const BOOL v2 = strcmp(argv[3], "v2") == 0;
	const UINT32 w = (UINT32)atoi(argv[4]);
	const UINT32 h = (UINT32)atoi(argv[5]);
	primitives_t* prims = primitives_get_generic();
	const size_t fsz = (size_t)w * h * 3 / 2;
	BYTE* main = calloc(1, fsz);
	BYTE* aux = calloc(1, fsz);
	BYTE* pMain[3] = { main, main + w * h, main + w * h + (w / 2) * (h / 2) };
	BYTE* pAux[3] = { aux, aux + w * h, aux + w * h + (w / 2) * (h / 2) };
	const UINT32 step[3] = { w, w / 2, w / 2 };
	const prim_size_t roi = { w, h };
	/* FreeRDP's RGBToAVC444YUV reads BGRX; swap R/B in place. */
	for (size_t i = 0; i < (size_t)w * h; i++)
	{
		BYTE t = rgba[i * 4];
		rgba[i * 4] = rgba[i * 4 + 2];
		rgba[i * 4 + 2] = t;
	}
	const pstatus_t rc =
	    v2 ? prims->RGBToAVC444YUVv2(rgba, PIXEL_FORMAT_BGRX32, w * 4, pMain, step, pAux, step, &roi)
	       : prims->RGBToAVC444YUV(rgba, PIXEL_FORMAT_BGRX32, w * 4, pMain, step, pAux, step, &roi);
	if (rc != PRIMITIVES_SUCCESS)
		return 3;
	write_file(argv[6], main, fsz);
	write_file(argv[7], aux, fsz);
	return 0;
}

static int codec(int argc, char** argv)
{
	if (argc < 6)
		return 1;
	size_t len = 0;
	BYTE* data = read_file(argv[2], &len);
	const UINT32 w = (UINT32)atoi(argv[3]);
	const UINT32 h = (UINT32)atoi(argv[4]);
	BYTE* rgba = calloc(4, (size_t)w * h);
	BOOL ok = FALSE;

	if (strcmp(argv[1], "nsc") == 0)
	{
		NSC_CONTEXT* nsc = nsc_context_new();
		ok = nsc_process_message(nsc, 32, w, h, data, (UINT32)len, rgba, PIXEL_FORMAT_RGBA32, w * 4,
		                         0, 0, w, h, FREERDP_FLIP_VERTICAL);
	}
	else if (strcmp(argv[1], "clear") == 0)
	{
		CLEAR_CONTEXT* clear = clear_context_new(FALSE);
		ok = clear_decompress(clear, data, (UINT32)len, w, h, rgba, PIXEL_FORMAT_RGBA32, w * 4, 0, 0,
		                      w, h, NULL) >= 0;
	}
	else if (strcmp(argv[1], "progressive") == 0)
	{
		PROGRESSIVE_CONTEXT* progressive = progressive_context_new(FALSE);
		REGION16 invalid = { 0 };
		region16_init(&invalid);
		ok = progressive && progressive_create_surface_context(progressive, 0, w, h) >= 0 &&
		     progressive_decompress(progressive, data, (UINT32)len, rgba, PIXEL_FORMAT_RGBA32, w * 4, 0,
		                            0, &invalid, 0, 0) >= 0;
		region16_uninit(&invalid);
	}
	else if (strcmp(argv[1], "planar") == 0)
	{
		BITMAP_PLANAR_CONTEXT* planar = freerdp_bitmap_planar_context_new(0, w, h);
		ok = planar_decompress(planar, data, (UINT32)len, w, h, rgba, PIXEL_FORMAT_RGBA32, w * 4, 0,
		                       0, w, h, FALSE);
	}
	if (!ok)
	{
		fprintf(stderr, "%s decode failed\n", argv[1]);
		return 3;
	}
	for (size_t i = 0; i < (size_t)w * h; i++)
		rgba[i * 4 + 3] = 0xFF;
	write_file(argv[5], rgba, (size_t)w * h * 4);
	return 0;
}

int main(int argc, char** argv)
{
	if (argc < 2)
		return 1;
	if (strcmp(argv[1], "avc-combine") == 0)
		return avc_combine(argc, argv);
	if (strcmp(argv[1], "avc-split") == 0)
		return avc_split(argc, argv);
	return codec(argc, argv);
}
