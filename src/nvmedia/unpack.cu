// Exact sample replication: UYVY422 (camera) or planar Y444 (validation) to NV24.
// Surface stores use byte coordinates; the UV plane stores U then V in memory.
extern "C" __global__ void unpack(const unsigned char *src, unsigned pitch,
                                  unsigned planar, unsigned long long y_surface,
                                  unsigned long long uv_surface) {
    unsigned x = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned y = blockIdx.y * blockDim.y + threadIdx.y;
    if (x >= 1920 || y >= 1080) return;
    unsigned luma, u, v;
    if (planar) {
        unsigned i = y * 1920 + x;
        luma = src[i]; u = src[1920 * 1080 + i]; v = src[2 * 1920 * 1080 + i];
    } else {
        unsigned i = y * pitch + (x & ~1u) * 2;
        u = src[i]; v = src[i + 2]; luma = src[i + 1 + (x & 1u) * 2];
    }
    unsigned uv = u | (v << 8);
    unsigned uv_x = x * 2;
    asm volatile("sust.b.2d.b8.trap [%0, {%1,%2}], %3;" :: "l"(y_surface), "r"(x), "r"(y), "r"(luma));
    asm volatile("sust.b.2d.b16.trap [%0, {%1,%2}], %3;" :: "l"(uv_surface), "r"(uv_x), "r"(y), "h"((unsigned short)uv));
}
