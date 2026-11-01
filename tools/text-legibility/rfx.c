/* RemoteFX Progressive round trip, the way krdp's progressive mode sends a frame
 * (VideoStream::sendFrameProgressive) and FreeRDP's client decodes it.
 *   rfx IN.bgrx OUT.bgrx WIDTH HEIGHT
 * Raw BGRX in and out (ffmpeg -pix_fmt bgr0 makes and reads them). docs/rdp-text.md */
#include <stdio.h>
#include <stdlib.h>
#include <freerdp/codec/progressive.h>
#include <freerdp/codec/region.h>

int main(int argc, char** argv)
{
	if (argc != 5)
		return fprintf(stderr, "usage: rfx IN.bgrx OUT.bgrx WIDTH HEIGHT\n"), 2;
	UINT32 w = (UINT32)atoi(argv[3]), h = (UINT32)atoi(argv[4]), stride = w * 4, size = stride * h;
	BYTE* src = malloc(size);
	BYTE* dst = calloc(1, size);
	FILE* f = fopen(argv[1], "rb");
	if (!f || fread(src, 1, size, f) != size)
		return fprintf(stderr, "can't read %s\n", argv[1]), 1;
	fclose(f);
	PROGRESSIVE_CONTEXT* enc = progressive_context_new(TRUE);
	PROGRESSIVE_CONTEXT* dec = progressive_context_new(FALSE);
	REGION16 all;
	region16_init(&all);
	RECTANGLE_16 r = { 0, 0, (UINT16)w, (UINT16)h };
	region16_union_rect(&all, &all, &r);
	BYTE* data = NULL;
	UINT32 len = 0;
	if (progressive_compress(enc, src, size, PIXEL_FORMAT_BGRX32, w, h, stride, &all, &data, &len) < 0)
		return fprintf(stderr, "compress failed\n"), 1;
	if (progressive_create_surface_context(dec, 1, w, h) < 0)
		return fprintf(stderr, "surface failed\n"), 1;
	REGION16 out;
	region16_init(&out);
	if (progressive_decompress(dec, data, len, dst, PIXEL_FORMAT_BGRX32, stride, 0, 0, &out, 1, 0) < 0)
		return fprintf(stderr, "decompress failed\n"), 1;
	f = fopen(argv[2], "wb");
	fwrite(dst, 1, size, f);
	fclose(f);
	fprintf(stderr, "progressive: %u bytes for %ux%u\n", len, w, h);
	return 0;
}
